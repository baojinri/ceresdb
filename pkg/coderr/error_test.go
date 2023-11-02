/*
 * Copyright 2022 The CeresDB Authors
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

package coderr

import (
	"fmt"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestErrorStack(t *testing.T) {
	r := require.New(t)
	cerr := NewCodeError(Internal, "test internal error")
	err := cerr.WithCausef("failed reason:%s", "for test")
	errDesc := fmt.Sprintf("%s", err)
	expectErrDesc := "ceresmeta/pkg/coderr/error_test.go:"

	r.True(strings.Contains(errDesc, expectErrDesc), "actual errDesc:%s", errDesc)
}
