// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

pub mod local_runner;
pub mod node_picker;
mod remote_client;
pub mod remote_runner;

use std::sync::Arc;

use async_trait::async_trait;
use common_types::{request_id::RequestId, schema::Schema, SequenceNumber};
use generic_error::{BoxError, GenericError};
use macros::define_result;
use object_store::Path;
use snafu::{Backtrace, OptionExt, ResultExt, Snafu};
use table_engine::table::TableId;

use crate::{
    compaction::CompactionInputFiles,
    instance::flush_compaction,
    row_iter::IterOptions,
    space::SpaceId,
    sst::{
        factory::SstWriteOptions,
        writer::{MetaData, SstInfo},
    },
    table::data::TableData,
};

/// Compaction runner
#[async_trait]
pub trait CompactionRunner: Send + Sync + 'static {
    async fn run(
        &self,
        task: CompactionRunnerTask,
    ) -> flush_compaction::Result<CompactionRunnerResult>;
}

pub type CompactionRunnerPtr = Box<dyn CompactionRunner>;
pub type CompactionRunnerRef = Arc<dyn CompactionRunner>;

#[derive(Debug, Snafu)]
#[snafu(visibility = "pub")]
pub enum Error {
    #[snafu(display("Empty table schema.\nBacktrace:\n{}", backtrace))]
    EmptyTableSchema { backtrace: Backtrace },

    #[snafu(display("Empty input context.\nBacktrace:\n{}", backtrace))]
    EmptyInputContext { backtrace: Backtrace },

    #[snafu(display("Empty ouput context.\nBacktrace:\n{}", backtrace))]
    EmptyOuputContext { backtrace: Backtrace },

    #[snafu(display("Empty compaction input files.\nBacktrace:\n{}", backtrace))]
    EmptyCompactionInputFiles { backtrace: Backtrace },

    #[snafu(display("Empty write options.\nBacktrace:\n{}", backtrace))]
    EmptySstWriteOptions { backtrace: Backtrace },

    #[snafu(display("Sst meta data is empty.\nBacktrace:\n{backtrace}"))]
    EmptySstMeta { backtrace: Backtrace },

    #[snafu(display("Empty sst info.\nBacktrace:\n{}", backtrace))]
    EmptySstInfo { backtrace: Backtrace },

    #[snafu(display("Empty compaction task exec result.\nBacktrace:\n{}", backtrace))]
    EmptyExecResult { backtrace: Backtrace },

    #[snafu(display("Failed to convert table schema, err:{}", source))]
    ConvertTableSchema { source: GenericError },

    #[snafu(display("Failed to convert input context, err:{}", source))]
    ConvertInputContext { source: GenericError },

    #[snafu(display("Failed to convert ouput context, err:{}", source))]
    ConvertOuputContext { source: GenericError },

    #[snafu(display("Failed to convert compaction input files, err:{}", source))]
    ConvertCompactionInputFiles { source: GenericError },

    #[snafu(display("Failed to convert write options, err:{}", source))]
    ConvertSstWriteOptions { source: GenericError },

    #[snafu(display("Failed to convert sst info, err:{}", source))]
    ConvertSstInfo { source: GenericError },

    #[snafu(display("Failed to convert sst meta, err:{}", source))]
    ConvertSstMeta { source: GenericError },

    #[snafu(display("Failed to connect the service endpoint:{}, err:{}", addr, source,))]
    FailConnect { addr: String, source: GenericError },

    #[snafu(display("Failed to execute compaction task, err:{}", source))]
    FailExecuteCompactionTask { source: GenericError },

    #[snafu(display("Missing header in rpc response.\nBacktrace:\n{}", backtrace))]
    MissingHeader { backtrace: Backtrace },

    #[snafu(display(
        "Bad response, resp code:{}, msg:{}.\nBacktrace:\n{}",
        code,
        msg,
        backtrace
    ))]
    BadResponse {
        code: u32,
        msg: String,
        backtrace: Backtrace,
    },
}

define_result!(Error);

/// Compaction runner task
#[derive(Debug, Clone)]
pub struct CompactionRunnerTask {
    // TODO: unused now, will be used in remote compaction.
    #[allow(unused)]
    pub task_key: String,
    /// Trace id for this operation
    pub request_id: RequestId,

    pub schema: Schema,
    pub space_id: SpaceId,
    pub table_id: TableId,
    pub sequence: SequenceNumber,

    /// Input context
    pub input_ctx: InputContext,
    /// Output context
    pub output_ctx: OutputContext,
}

impl CompactionRunnerTask {
    pub(crate) fn new(
        request_id: RequestId,
        input_files: CompactionInputFiles,
        table_data: &TableData,
        file_id: u64,
        sst_write_options: SstWriteOptions,
    ) -> Self {
        // Create task key.
        let task_key = table_data.compaction_task_key(file_id);

        // Create executor task.
        let table_options = table_data.table_options();

        let input_ctx = {
            let iter_options = IterOptions {
                batch_size: table_options.num_rows_per_row_group,
            };

            InputContext {
                files: input_files,
                num_rows_per_row_group: table_options.num_rows_per_row_group,
                merge_iter_options: iter_options,
                need_dedup: table_options.need_dedup(),
            }
        };

        let output_ctx = {
            let file_path = table_data.sst_file_path(file_id);
            OutputContext {
                file_path,
                write_options: sst_write_options,
            }
        };

        Self {
            task_key,
            request_id,
            schema: table_data.schema(),
            space_id: table_data.space_id,
            table_id: table_data.id,
            sequence: table_data.last_sequence(),
            input_ctx,
            output_ctx,
        }
    }
}

impl TryFrom<horaedbproto::compaction_service::ExecuteCompactionTaskRequest>
    for CompactionRunnerTask
{
    type Error = Error;

    fn try_from(
        request: horaedbproto::compaction_service::ExecuteCompactionTaskRequest,
    ) -> Result<Self> {
        let task_key = request.task_key;
        let request_id: RequestId = request.request_id.into();

        let schema: Schema = request
            .schema
            .context(EmptyTableSchema)?
            .try_into()
            .box_err()
            .context(ConvertTableSchema)?;

        let space_id: SpaceId = request.space_id;
        let table_id: TableId = request.table_id.into();
        let sequence: SequenceNumber = request.sequence;

        let input_ctx: InputContext = request
            .input_ctx
            .context(EmptyInputContext)?
            .try_into()
            .box_err()
            .context(ConvertInputContext)?;

        let output_ctx: OutputContext = request
            .output_ctx
            .context(EmptyOuputContext)?
            .try_into()
            .box_err()
            .context(ConvertOuputContext)?;

        Ok(Self {
            task_key,
            request_id,
            schema,
            space_id,
            table_id,
            sequence,
            input_ctx,
            output_ctx,
        })
    }
}

impl From<CompactionRunnerTask> for horaedbproto::compaction_service::ExecuteCompactionTaskRequest {
    fn from(task: CompactionRunnerTask) -> Self {
        Self {
            task_key: task.task_key,
            request_id: task.request_id.into(),
            schema: Some((&(task.schema)).into()),
            space_id: task.space_id,
            table_id: task.table_id.into(),
            sequence: task.sequence,
            input_ctx: Some(task.input_ctx.into()),
            output_ctx: Some(task.output_ctx.into()),
        }
    }
}

pub struct CompactionRunnerResult {
    pub output_file_path: Path,
    pub sst_info: SstInfo,
    pub sst_meta: MetaData,
}

impl TryFrom<horaedbproto::compaction_service::ExecuteCompactionTaskResponse>
    for CompactionRunnerResult
{
    type Error = Error;

    fn try_from(
        resp: horaedbproto::compaction_service::ExecuteCompactionTaskResponse,
    ) -> Result<Self> {
        let res = resp.result.context(EmptyExecResult)?;
        let sst_info = res
            .sst_info
            .context(EmptySstInfo)?
            .try_into()
            .box_err()
            .context(ConvertSstInfo)?;
        let sst_meta = res
            .sst_meta
            .context(EmptySstMeta)?
            .try_into()
            .box_err()
            .context(ConvertSstMeta)?;

        Ok(Self {
            output_file_path: res.output_file_path.into(),
            sst_info,
            sst_meta,
        })
    }
}

#[derive(Debug, Clone)]
pub struct InputContext {
    /// Input sst files in this compaction
    pub files: CompactionInputFiles,
    pub num_rows_per_row_group: usize,
    pub merge_iter_options: IterOptions,
    pub need_dedup: bool,
}

impl TryFrom<horaedbproto::compaction_service::InputContext> for InputContext {
    type Error = Error;

    fn try_from(value: horaedbproto::compaction_service::InputContext) -> Result<Self> {
        let num_rows_per_row_group: usize = value.num_rows_per_row_group as usize;
        let merge_iter_options = IterOptions {
            batch_size: value.merge_iter_options as usize,
        };
        let need_dedup = value.need_dedup;

        let files: CompactionInputFiles = value
            .files
            .context(EmptyCompactionInputFiles)?
            .try_into()
            .box_err()
            .context(ConvertCompactionInputFiles)?;

        Ok(InputContext {
            files,
            num_rows_per_row_group,
            merge_iter_options,
            need_dedup,
        })
    }
}

impl From<InputContext> for horaedbproto::compaction_service::InputContext {
    fn from(value: InputContext) -> Self {
        Self {
            files: Some(value.files.into()),
            num_rows_per_row_group: value.num_rows_per_row_group as u64,
            merge_iter_options: value.merge_iter_options.batch_size as u64,
            need_dedup: value.need_dedup,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutputContext {
    /// Output sst file path
    pub file_path: Path,
    /// Output sst write context
    pub write_options: SstWriteOptions,
}

impl TryFrom<horaedbproto::compaction_service::OutputContext> for OutputContext {
    type Error = Error;

    fn try_from(value: horaedbproto::compaction_service::OutputContext) -> Result<Self> {
        let file_path: Path = value.file_path.into();
        let write_options: SstWriteOptions = value
            .write_options
            .context(EmptySstWriteOptions)?
            .try_into()
            .box_err()
            .context(ConvertSstWriteOptions)?;

        Ok(OutputContext {
            file_path,
            write_options,
        })
    }
}

impl From<OutputContext> for horaedbproto::compaction_service::OutputContext {
    fn from(value: OutputContext) -> Self {
        Self {
            file_path: value.file_path.into(),
            write_options: Some(value.write_options.into()),
        }
    }
}
