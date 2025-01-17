// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use itertools::Itertools;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use super::config::{EmbeddedPythonConfig, RawAllocator, RunMode, TerminfoResolution};

/// Obtain the Rust source code to construct a PythonConfig instance.
pub fn derive_python_config(
    embedded: &EmbeddedPythonConfig,
    run_mode: &RunMode,
    importlib_bootstrap_path: &PathBuf,
    importlib_bootstrap_external_path: &PathBuf,
    py_modules_path: &PathBuf,
    py_resources_path: &PathBuf,
) -> String {
    format!(
        "PythonConfig {{\n    \
         standard_io_encoding: {},\n    \
         standard_io_errors: {},\n    \
         opt_level: {},\n    \
         use_custom_importlib: true,\n    \
         filesystem_importer: {},\n    \
         sys_paths: [{}].to_vec(),\n    \
         bytes_warning: {},\n    \
         import_site: {},\n    \
         import_user_site: {},\n    \
         ignore_python_env: {},\n    \
         inspect: {},\n    \
         interactive: {},\n    \
         isolated: {},\n    \
         legacy_windows_fs_encoding: {},\n    \
         legacy_windows_stdio: {},\n    \
         dont_write_bytecode: {},\n    \
         unbuffered_stdio: {},\n    \
         parser_debug: {},\n    \
         quiet: {},\n    \
         use_hash_seed: {},\n    \
         verbose: {},\n    \
         frozen_importlib_data: include_bytes!(r#\"{}\"#),\n    \
         frozen_importlib_external_data: include_bytes!(r#\"{}\"#),\n    \
         py_modules_data: include_bytes!(r#\"{}\"#),\n    \
         py_resources_data: include_bytes!(r#\"{}\"#),\n    \
         extra_extension_modules: vec![],\n    \
         argvb: false,\n    \
         sys_frozen: {},\n    \
         sys_meipass: {},\n    \
         raw_allocator: {},\n    \
         terminfo_resolution: {},\n    \
         write_modules_directory_env: {},\n    \
         run: {},\n\
         }}",
        match &embedded.stdio_encoding_name {
            Some(value) => format_args!("Some(\"{}\")", value).to_string(),
            None => "None".to_owned(),
        },
        match &embedded.stdio_encoding_errors {
            Some(value) => format_args!("Some(\"{}\")", value).to_string(),
            None => "None".to_owned(),
        },
        embedded.optimize_level,
        embedded.filesystem_importer,
        &embedded
            .sys_paths
            .iter()
            .map(|p| "\"".to_owned() + p + "\".to_string()")
            .collect::<Vec<String>>()
            .join(", "),
        embedded.bytes_warning,
        !embedded.no_site,
        !embedded.no_user_site_directory,
        embedded.ignore_environment,
        embedded.inspect,
        embedded.interactive,
        embedded.isolated,
        embedded.legacy_windows_fs_encoding,
        embedded.legacy_windows_stdio,
        embedded.dont_write_bytecode,
        embedded.unbuffered_stdio,
        embedded.parser_debug,
        embedded.quiet,
        embedded.use_hash_seed,
        embedded.verbose,
        importlib_bootstrap_path.display(),
        importlib_bootstrap_external_path.display(),
        py_modules_path.display(),
        py_resources_path.display(),
        embedded.sys_frozen,
        embedded.sys_meipass,
        match embedded.raw_allocator {
            RawAllocator::Jemalloc => "PythonRawAllocator::Jemalloc",
            RawAllocator::Rust => "PythonRawAllocator::Rust",
            RawAllocator::System => "PythonRawAllocator::System",
        },
        match embedded.terminfo_resolution {
            TerminfoResolution::Dynamic => "TerminfoResolution::Dynamic".to_string(),
            TerminfoResolution::None => "TerminfoResolution::None".to_string(),
            TerminfoResolution::Static(ref v) => {
                format!("TerminfoResolution::Static(r###\"{}\"###", v)
            }
        },
        match &embedded.write_modules_directory_env {
            Some(path) => "Some(\"".to_owned() + &path + "\".to_string())",
            _ => "None".to_owned(),
        },
        match run_mode {
            RunMode::Noop => "PythonRunMode::None".to_owned(),
            RunMode::Repl => "PythonRunMode::Repl".to_owned(),
            RunMode::Module { ref module } => {
                "PythonRunMode::Module { module: \"".to_owned() + module + "\".to_string() }"
            }
            RunMode::Eval { ref code } => {
                "PythonRunMode::Eval { code: r###\"".to_owned() + code + "\"###.to_string() }"
            }
        },
    )
}

pub fn write_data_rs(path: &PathBuf, python_config_rs: &str) {
    let mut f = File::create(&path).unwrap();

    f.write_all(b"use super::config::{PythonConfig, PythonRawAllocator, PythonRunMode, TerminfoResolution};\n\n")
        .unwrap();

    // Ideally we would have a const struct, but we need to do some
    // dynamic allocations. Using a function avoids having to pull in a
    // dependency on lazy_static.
    let indented = python_config_rs
        .split('\n')
        .map(|line| "    ".to_owned() + line)
        .join("\n");

    f.write_fmt(format_args!(
        "/// Obtain the default Python configuration\n\
         ///\n\
         /// The crate is compiled with a default Python configuration embedded
         /// in the crate. This function will return an instance of that
         /// configuration.
         pub fn default_python_config() -> PythonConfig {{\n{}\n}}\n",
        indented
    ))
    .unwrap();
}
