/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::io::Write;

use tempfile::NamedTempFile;

pub struct TmpManifests {
    pub autocargo_file: NamedTempFile,
    pub autocargo_lib_file: NamedTempFile,
    pub codegen_includer_proc_macro_file: NamedTempFile,
    pub thrift_compiler_file: NamedTempFile,
    pub thrift_test_file: NamedTempFile,
}

impl TmpManifests {
    pub fn new() -> Self {
        let create = |bytes| {
            let mut file = NamedTempFile::new().unwrap();
            file.write_all(bytes).unwrap();
            file.flush().unwrap();
            file
        };
        Self {
            autocargo_file: create(include_bytes!(
                "../../buck_generated/autocargo_rust_manifest.json"
            )),
            autocargo_lib_file: create(include_bytes!(
                "../../buck_generated/autocargo_lib_rust_manifest.json"
            )),
            codegen_includer_proc_macro_file: create(include_bytes!(
                "../../buck_generated/codegen_includer_proc_macro_rust_manifest.json"
            )),
            thrift_compiler_file: create(include_bytes!(
                "../../buck_generated/thrift_compiler_rust_manifest.json"
            )),
            thrift_test_file: create(include_bytes!(
                "../../buck_generated/thrift_test_rust_manifest.json"
            )),
        }
    }
}
