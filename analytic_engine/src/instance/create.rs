// Copyright 2022-2023 CeresDB Project Authors. Licensed under Apache-2.0.

//! Create table logic of instance

use std::sync::Arc;

use common_util::error::BoxError;
use log::info;
use snafu::ResultExt;
use table_engine::engine::CreateTableRequest;

use crate::{
    instance::{
        engine::{CreateOpenFailedTable, CreateTableData, InvalidOptions, Result, WriteManifest},
        Instance,
    },
    manifest::meta_update::{AddTableMeta, MetaUpdate, MetaUpdateRequest},
    space::SpaceRef,
    table::data::{TableData, TableDataRef},
    table_options,
};

impl Instance {
    /// Create table need to be handled by write worker.
    pub async fn do_create_table(
        &self,
        space: SpaceRef,
        request: CreateTableRequest,
    ) -> Result<TableDataRef> {
        info!("Instance create table, request:{:?}", request);

        if space.is_open_failed_table(&request.table_name) {
            return CreateOpenFailedTable {
                table: request.table_name,
            }
            .fail();
        }

        let mut table_opts =
            table_options::merge_table_options_for_create(&request.options, &self.table_opts)
                .box_err()
                .context(InvalidOptions {
                    space_id: space.id,
                    table: &request.table_name,
                    table_id: request.table_id,
                })?;
        // Sanitize options before creating table.
        table_opts.sanitize();

        if let Some(table_data) = space.find_table_by_id(request.table_id) {
            return Ok(table_data);
        }

        // Choose a write worker for this table
        let (table_name, table_id) = (request.table_name.clone(), request.table_id);

        let table_data = Arc::new(
            TableData::new(
                space.id,
                request,
                table_opts,
                &self.file_purger,
                self.preflush_write_buffer_size_ratio,
                space.mem_usage_collector.clone(),
            )
            .context(CreateTableData {
                space_id: space.id,
                table: &table_name,
                table_id,
            })?,
        );

        // Store table info into meta
        let update_req = {
            let meta_update = MetaUpdate::AddTable(AddTableMeta {
                space_id: space.id,
                table_id: table_data.id,
                table_name: table_data.name.clone(),
                schema: table_data.schema(),
                opts: table_data.table_options().as_ref().clone(),
            });
            MetaUpdateRequest {
                shard_info: table_data.shard_info,
                meta_update,
            }
        };
        self.space_store
            .manifest
            .store_update(update_req)
            .await
            .context(WriteManifest {
                space_id: space.id,
                table: &table_data.name,
                table_id: table_data.id,
            })?;

        space.insert_table(table_data.clone());
        Ok(table_data)
    }
}
