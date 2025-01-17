// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use glob::glob as findglob;
use lazy_static::lazy_static;
use slog::{info, warn};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::fs::create_dir_all;
use std::io::{BufRead, BufReader, BufWriter, Error as IOError, Write};
use std::path::{Path, PathBuf};

use super::config::{
    eval_starlark_config_file, find_pyoxidizer_config_file_env, Config, PythonPackaging,
};
use super::packaging_rule::{
    packages_from_module_names, resolve_python_packaging, ResourceAction, ResourceLocation,
};
use super::state::{BuildContext, PackagingState};
use crate::py_packaging::bytecode::{python_source_encoding, BytecodeCompiler, CompileMode};
use crate::py_packaging::distribution::{
    resolve_python_distribution_archive, ExtensionModule, ParsedPythonDistribution,
    PythonDistributionLocation,
};
use crate::py_packaging::embedded_resource::EmbeddedPythonResources;
use crate::py_packaging::libpython::{derive_importlib, link_libpython};
use crate::py_packaging::pyembed::{derive_python_config, write_data_rs};
use crate::py_packaging::resource::{
    packages_from_module_name, AppRelativeResources, PackagedModuleBytecode, PackagedModuleSource,
    PythonResource,
};

lazy_static! {
    /// Python extension modules that should never be included.
    ///
    /// Ideally this data structure doesn't exist. But there are some problems
    /// with various extensions on various targets.
    static ref OS_IGNORE_EXTENSIONS: Vec<&'static str> = {
        let mut v = Vec::new();

        if cfg!(target_os = "linux") {
            // Linking issues.
            v.push("_crypt");

            // Linking issues.
            v.push("nis");
        }

        else if cfg!(target_os = "macos") {
            // curses and readline have linking issues.
            v.push("_curses");
            v.push("_curses_panel");
            v.push("readline");
        }

        v
    };
}

pub const HOST: &str = env!("HOST");

impl BuildContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        logger: &slog::Logger,
        project_path: &Path,
        config_path: &Path,
        host: Option<&str>,
        target: &str,
        release: bool,
        force_artifacts_path: Option<&Path>,
        verbose: bool,
    ) -> Result<Self, String> {
        let config_parent_path = config_path
            .parent()
            .ok_or("could not resolve parent path of config".to_string())?;

        let host_triple = if let Some(v) = host {
            v.to_string()
        } else {
            HOST.to_string()
        };

        let config = eval_starlark_config_file(logger, &config_path, target)?;

        let build_path = config.build_config.build_path.clone();

        // Build Rust artifacts into build path, not wherever Rust chooses.
        let target_base_path = build_path.join("target");

        let apps_base_path = build_path.join("apps");

        // This assumes we invoke as `cargo build --target`, otherwise we don't get the
        // target triple in the directory path unless cross compiling.
        let target_triple_base_path =
            target_base_path
                .join(target)
                .join(if release { "release" } else { "debug" });

        let app_name = config.build_config.application_name.clone();

        let exe_name = if target.contains("pc-windows") {
            format!("{}.exe", &app_name)
        } else {
            app_name.clone()
        };

        let app_target_path = target_triple_base_path.join(&app_name);

        let app_path = apps_base_path
            .join(&app_name)
            .join(target)
            .join(if release { "release" } else { "debug" });
        let app_exe_target_path = target_triple_base_path.join(&exe_name);
        let app_exe_path = app_path.join(&exe_name);

        // Artifacts path is:
        // 1. force_artifacts_path (if defined)
        // 2. A "pyoxidizer" directory in the target directory.
        let pyoxidizer_artifacts_path = match force_artifacts_path {
            Some(path) => path.to_path_buf(),
            None => target_triple_base_path.join("pyoxidizer"),
        };

        let distributions_path = build_path.join("distribution");

        let distribution_hash = match &config.python_distribution {
            PythonDistributionLocation::Local { sha256, .. } => sha256,
            PythonDistributionLocation::Url { sha256, .. } => sha256,
        };

        // Take the prefix so paths are shorter.
        let distribution_hash = &distribution_hash[0..12];

        let python_distribution_path =
            pyoxidizer_artifacts_path.join(format!("python.{}", distribution_hash));

        let cargo_toml_path = project_path.join("Cargo.toml");
        if !cargo_toml_path.exists() {
            return Err(format!("{} does not exist", cargo_toml_path.display()));
        }

        let cargo_toml_data = fs::read(&cargo_toml_path).or_else(|e| Err(e.to_string()))?;
        let cargo_config =
            cargo_toml::Manifest::from_slice(&cargo_toml_data).or_else(|e| Err(e.to_string()))?;

        Ok(BuildContext {
            project_path: project_path.to_path_buf(),
            config_path: config_path.to_path_buf(),
            config_parent_path: config_parent_path.to_path_buf(),
            config,
            cargo_config,
            verbose,
            build_path,
            app_name,
            app_path,
            app_exe_path,
            distributions_path,
            host_triple,
            target_triple: target.to_string(),
            release,
            target_base_path,
            target_triple_base_path,
            app_target_path,
            app_exe_target_path,
            pyoxidizer_artifacts_path,
            python_distribution_path,
            packaging_state: None,
        })
    }

    /// Obtain the PackagingState instance for this configuration.
    ///
    /// This basically reads the packaging_state.cbor file from the artifacts
    /// directory.
    pub fn get_packaging_state(&mut self) -> Result<PackagingState, String> {
        if self.packaging_state.is_none() {
            let path = self.pyoxidizer_artifacts_path.join("packaging_state.cbor");
            let fh = std::io::BufReader::new(
                std::fs::File::open(&path).or_else(|e| Err(e.to_string()))?,
            );

            let state: PackagingState =
                serde_cbor::from_reader(fh).or_else(|e| Err(e.to_string()))?;

            self.packaging_state = Some(state);
        }

        // Ideally we'd return a ref. But lifetimes and mutable borrows can get
        // tricky. So just stomach the clone() for now.
        Ok(self.packaging_state.clone().unwrap())
    }
}

/// Represents resources to package with an application.
#[derive(Debug)]
pub struct PythonResources {
    /// Resources to be embedded in the binary.
    pub embedded: EmbeddedPythonResources,

    /// Resources to install in paths relative to the produced binary.
    pub app_relative: BTreeMap<String, AppRelativeResources>,

    /// Files that are read to resolve this data structure.
    pub read_files: Vec<PathBuf>,

    /// Path where to write license files.
    pub license_files_path: Option<String>,
}

fn read_resource_names_file(path: &Path) -> Result<BTreeSet<String>, IOError> {
    let fh = fs::File::open(path)?;

    let mut res: BTreeSet<String> = BTreeSet::new();

    for line in BufReader::new(fh).lines() {
        let line = line?;

        if line.starts_with('#') || line.is_empty() {
            continue;
        }

        res.insert(line);
    }

    Ok(res)
}

fn filter_btreemap<V>(logger: &slog::Logger, m: &mut BTreeMap<String, V>, f: &BTreeSet<String>) {
    let keys: Vec<String> = m.keys().cloned().collect();

    for key in keys {
        if !f.contains(&key) {
            warn!(logger, "removing {}", key);
            m.remove(&key);
        }
    }
}

struct BytecodeRequest {
    source: Vec<u8>,
    optimize_level: i32,
    is_package: bool,
}

/// Resolves a series of packaging rules to a final set of resources to package.
#[allow(clippy::cognitive_complexity)]
pub fn resolve_python_resources(
    logger: &slog::Logger,
    context: &BuildContext,
    dist: &ParsedPythonDistribution,
) -> PythonResources {
    let packages = &context.config.python_packaging;

    // Since bytecode has a non-trivial cost to generate, our strategy is to accumulate
    // requests for bytecode then generate bytecode for the final set of inputs at the
    // end of processing. That way we don't generate bytecode only to throw it away later.

    let mut embedded_extension_modules: BTreeMap<String, ExtensionModule> = BTreeMap::new();
    let mut embedded_sources: BTreeMap<String, PackagedModuleSource> = BTreeMap::new();
    let mut embedded_bytecode_requests: BTreeMap<String, BytecodeRequest> = BTreeMap::new();
    let mut embedded_resources: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    let mut embedded_built_extension_modules = BTreeMap::new();

    let mut app_relative: BTreeMap<String, AppRelativeResources> = BTreeMap::new();
    let mut app_relative_bytecode_requests: BTreeMap<String, BTreeMap<String, BytecodeRequest>> =
        BTreeMap::new();

    let mut read_files: Vec<PathBuf> = Vec::new();
    let mut license_files_path = None;

    for packaging in packages {
        warn!(logger, "processing packaging rule: {:?}", packaging);

        let verbose_rule = if let PythonPackaging::Stdlib(_) = packaging {
            true
        } else {
            false
        };

        for entry in resolve_python_packaging(logger, packaging, dist) {
            match (entry.action, entry.location, entry.resource) {
                (
                    ResourceAction::Add,
                    ResourceLocation::Embedded,
                    PythonResource::ExtensionModule { name, module },
                ) => {
                    warn!(logger, "adding embedded extension module: {}", name);
                    embedded_extension_modules.insert(name, module);
                }
                (
                    ResourceAction::Add,
                    ResourceLocation::AppRelative { .. },
                    PythonResource::ExtensionModule { .. },
                ) => {
                    panic!("should not have gotten an app-relative extension module");
                }
                (
                    ResourceAction::Remove,
                    ResourceLocation::Embedded,
                    PythonResource::ExtensionModule { name, .. },
                ) => {
                    warn!(logger, "removing embedded extension module: {}", name);
                    embedded_extension_modules.remove(&name);
                }
                (
                    ResourceAction::Add,
                    ResourceLocation::Embedded,
                    PythonResource::ModuleSource {
                        name,
                        source,
                        is_package,
                    },
                ) => {
                    if verbose_rule {
                        info!(logger, "adding embedded module source: {}", name);
                    } else {
                        warn!(logger, "adding embedded module source: {}", name);
                    }
                    embedded_sources
                        .insert(name.clone(), PackagedModuleSource { source, is_package });
                }
                (
                    ResourceAction::Add,
                    ResourceLocation::AppRelative { path },
                    PythonResource::ModuleSource {
                        name,
                        source,
                        is_package,
                    },
                ) => {
                    if verbose_rule {
                        info!(
                            logger,
                            "adding app-relative module source to {}: {}", path, name
                        );
                    } else {
                        warn!(
                            logger,
                            "adding app-relative module source to {}: {}", path, name
                        );
                    }
                    if !app_relative.contains_key(&path) {
                        app_relative.insert(path.clone(), AppRelativeResources::default());
                    }

                    app_relative
                        .get_mut(&path)
                        .unwrap()
                        .module_sources
                        .insert(name.clone(), PackagedModuleSource { source, is_package });
                }
                (
                    ResourceAction::Remove,
                    ResourceLocation::Embedded,
                    PythonResource::ModuleSource { name, .. },
                ) => {
                    warn!(logger, "removing embedded module source: {}", name);
                    embedded_sources.remove(&name);
                }
                (
                    ResourceAction::Add,
                    ResourceLocation::Embedded,
                    PythonResource::ModuleBytecodeRequest {
                        name,
                        source,
                        optimize_level,
                        is_package,
                    },
                ) => {
                    if verbose_rule {
                        info!(logger, "adding embedded module bytecode: {}", name);
                    } else {
                        warn!(logger, "adding embedded module bytecode: {}", name);
                    }
                    embedded_bytecode_requests.insert(
                        name.clone(),
                        BytecodeRequest {
                            source,
                            optimize_level,
                            is_package,
                        },
                    );
                }
                (
                    ResourceAction::Add,
                    ResourceLocation::AppRelative { path },
                    PythonResource::ModuleBytecodeRequest {
                        name,
                        source,
                        optimize_level,
                        is_package,
                    },
                ) => {
                    if verbose_rule {
                        info!(
                            logger,
                            "adding app-relative module bytecode to {}: {}", path, name
                        );
                    } else {
                        warn!(
                            logger,
                            "adding app-relative module bytecode to {}: {}", path, name
                        );
                    }

                    if !app_relative_bytecode_requests.contains_key(&path) {
                        app_relative_bytecode_requests.insert(path.clone(), BTreeMap::new());
                    }

                    app_relative_bytecode_requests
                        .get_mut(&path)
                        .unwrap()
                        .insert(
                            name.clone(),
                            BytecodeRequest {
                                source,
                                optimize_level,
                                is_package,
                            },
                        );
                }
                (
                    ResourceAction::Remove,
                    ResourceLocation::Embedded,
                    PythonResource::ModuleBytecodeRequest { name, .. },
                ) => {
                    warn!(logger, "removing embedded module bytecode: {}", name);
                    embedded_bytecode_requests.remove(&name);
                }
                (
                    ResourceAction::Add,
                    ResourceLocation::Embedded,
                    PythonResource::ModuleBytecode { .. },
                ) => {
                    panic!("adding embedded ModuleBytecode not supported");
                }
                (
                    ResourceAction::Remove,
                    ResourceLocation::Embedded,
                    PythonResource::ModuleBytecode { .. },
                ) => {
                    panic!("removing embedded ModuleBytecode not supported");
                }
                (
                    ResourceAction::Add,
                    ResourceLocation::AppRelative { .. },
                    PythonResource::ModuleBytecode { .. },
                ) => {
                    panic!("adding app-relative ModuleBytecode not supported");
                }
                (
                    ResourceAction::Remove,
                    ResourceLocation::AppRelative { .. },
                    PythonResource::ModuleBytecode { .. },
                ) => panic!("removing app-relative ModuleBytecode not supported"),
                (
                    ResourceAction::Add,
                    ResourceLocation::Embedded,
                    PythonResource::Resource {
                        package,
                        name,
                        data,
                    },
                ) => {
                    warn!(logger, "adding embedded resource: {} / {}", package, name);

                    if !embedded_resources.contains_key(&package) {
                        embedded_resources.insert(package.clone(), BTreeMap::new());
                    }

                    embedded_resources
                        .get_mut(&package)
                        .unwrap()
                        .insert(name, data);
                }
                (
                    ResourceAction::Add,
                    ResourceLocation::AppRelative { path },
                    PythonResource::Resource {
                        package,
                        name,
                        data,
                    },
                ) => {
                    warn!(logger, "adding app-relative resource to {}: {}", path, name);

                    if !app_relative.contains_key(&path) {
                        app_relative.insert(path.clone(), AppRelativeResources::default());
                    }

                    let app_relative = app_relative.get_mut(&path).unwrap();

                    if !app_relative.resources.contains_key(&package) {
                        app_relative
                            .resources
                            .insert(package.clone(), BTreeMap::new());
                    }

                    app_relative
                        .resources
                        .get_mut(&package)
                        .unwrap()
                        .insert(name, data);
                }
                (
                    ResourceAction::Remove,
                    ResourceLocation::Embedded,
                    PythonResource::Resource { name, .. },
                ) => {
                    warn!(logger, "removing embedded resource: {}", name);
                    embedded_resources.remove(&name);
                }
                (ResourceAction::Remove, ResourceLocation::AppRelative { .. }, _) => {
                    panic!("should not have gotten an action to remove an app-relative resource");
                }
                (
                    ResourceAction::Add,
                    ResourceLocation::Embedded,
                    PythonResource::BuiltExtensionModule(em),
                ) => {
                    warn!(
                        logger,
                        "adding embedded built extension module: {}", em.name
                    );

                    embedded_built_extension_modules.insert(em.name.clone(), em.clone());
                }
                (
                    ResourceAction::Add,
                    ResourceLocation::AppRelative { path },
                    PythonResource::BuiltExtensionModule(em),
                ) => {
                    warn!(
                        logger,
                        "adding app-relative built extension module {} to {}", em.name, path
                    );
                    warn!(
                        logger,
                        "WARNING: incomplete support for app-relative built extension modules: adding a built-in");
                    embedded_built_extension_modules.insert(em.name.clone(), em.clone());
                }
                (
                    ResourceAction::Remove,
                    ResourceLocation::Embedded,
                    PythonResource::BuiltExtensionModule(em),
                ) => {
                    warn!(
                        logger,
                        "removing embedded built extension module {}", em.name
                    );
                    embedded_built_extension_modules.remove(&em.name);
                }
            }
        }

        if let PythonPackaging::WriteLicenseFiles(rule) = packaging {
            license_files_path = Some(rule.path.clone());
        }

        if let PythonPackaging::FilterInclude(rule) = packaging {
            let mut include_names: BTreeSet<String> = BTreeSet::new();

            for path in &rule.files {
                let path = PathBuf::from(path);
                let new_names =
                    read_resource_names_file(&path).expect("failed to read resource names file");

                include_names.extend(new_names);
                read_files.push(path);
            }

            for glob in &rule.glob_files {
                let mut new_names: BTreeSet<String> = BTreeSet::new();

                for entry in findglob(glob).expect("glob_files glob match failed") {
                    match entry {
                        Ok(path) => {
                            new_names.extend(
                                read_resource_names_file(&path)
                                    .expect("failed to read resource names"),
                            );
                            read_files.push(path);
                        }
                        Err(e) => {
                            panic!("error reading resource names file: {:?}", e);
                        }
                    }
                }

                if new_names.is_empty() {
                    panic!(
                        "glob filter resolves to empty set; are you sure the paths are correct?"
                    );
                }

                include_names.extend(new_names);
            }

            warn!(
                logger,
                "filtering embedded extension modules from {:?}", packaging
            );
            filter_btreemap(logger, &mut embedded_extension_modules, &include_names);
            warn!(
                logger,
                "filtering embedded module sources from {:?}", packaging
            );
            filter_btreemap(logger, &mut embedded_sources, &include_names);
            warn!(
                logger,
                "filtering app-relative module sources from {:?}", packaging
            );
            for value in app_relative.values_mut() {
                filter_btreemap(logger, &mut value.module_sources, &include_names);
            }
            warn!(
                logger,
                "filtering embedded module bytecode from {:?}", packaging
            );
            filter_btreemap(logger, &mut embedded_bytecode_requests, &include_names);
            warn!(
                logger,
                "filtering app-relative module bytecode from {:?}", packaging
            );
            for value in app_relative_bytecode_requests.values_mut() {
                filter_btreemap(logger, value, &include_names);
            }
            warn!(logger, "filtering embedded resources from {:?}", packaging);
            filter_btreemap(logger, &mut embedded_resources, &include_names);
            warn!(
                logger,
                "filtering app-relative resources from {:?}", packaging
            );
            for value in app_relative.values_mut() {
                filter_btreemap(logger, &mut value.resources, &include_names);
            }
            warn!(
                logger,
                "filtering embedded built extension modules from {:?}", packaging
            );
            filter_btreemap(
                logger,
                &mut embedded_built_extension_modules,
                &include_names,
            );
        }
    }

    // Add empty modules for missing parent packages. This could happen if there are
    // namespace packages, for example.
    let mut missing_packages = BTreeSet::new();
    for name in embedded_bytecode_requests.keys() {
        for package in packages_from_module_name(&name) {
            if !embedded_bytecode_requests.contains_key(&package) {
                missing_packages.insert(package.clone());
            }
        }
    }

    for package in missing_packages {
        warn!(
            logger,
            "adding empty module for missing package {}", package
        );
        embedded_bytecode_requests.insert(
            package.clone(),
            BytecodeRequest {
                source: Vec::new(),
                optimize_level: 0,
                is_package: true,
            },
        );
    }

    // Add required extension modules, as some don't show up in the modules list
    // and may have been filtered or not added in the first place.
    for (name, variants) in &dist.extension_modules {
        let em = &variants[0];

        if (em.builtin_default || em.required) && !embedded_extension_modules.contains_key(name) {
            warn!(logger, "adding required embedded extension module {}", name);
            embedded_extension_modules.insert(name.clone(), em.clone());
        }
    }

    // Remove extension modules that have problems.
    for e in OS_IGNORE_EXTENSIONS.as_slice() {
        warn!(
            logger,
            "removing extension module due to incompatibility: {}", e
        );
        embedded_extension_modules.remove(&String::from(*e));
    }

    // Audit Python source for __file__, which could be problematic.
    let mut file_seen = false;

    for (name, request) in &embedded_bytecode_requests {
        // We can't just look for b"__file__ because the source file may be in
        // encodings like UTF-16. So we need to decode to Unicode first then look for
        // the code points.
        let encoding = python_source_encoding(&request.source);

        let encoder = match encoding_rs::Encoding::for_label(&encoding) {
            Some(encoder) => encoder,
            None => encoding_rs::UTF_8,
        };

        let (source, ..) = encoder.decode(&request.source);

        if source.contains("__file__") {
            warn!(logger, "warning: {} contains __file__", name);
            file_seen = true;
        }
    }

    if file_seen {
        warn!(logger, "__file__ was encountered in some modules; PyOxidizer does not set __file__ and this may create problems at run-time; see https://github.com/indygreg/PyOxidizer/issues/69 for more");
    }

    let mut embedded_bytecodes: BTreeMap<String, PackagedModuleBytecode> = BTreeMap::new();

    {
        let mut compiler = BytecodeCompiler::new(&dist.python_exe);

        for (name, request) in embedded_bytecode_requests {
            let bytecode = match compiler.compile(
                &request.source,
                &name,
                request.optimize_level,
                CompileMode::Bytecode,
            ) {
                Ok(res) => res,
                Err(msg) => panic!("error compiling bytecode for {}: {}", name, msg),
            };

            embedded_bytecodes.insert(
                name.clone(),
                PackagedModuleBytecode {
                    bytecode,
                    is_package: request.is_package,
                },
            );
        }
    }

    // Compile app-relative bytecode requests.
    {
        let mut compiler = BytecodeCompiler::new(&dist.python_exe);

        for (path, requests) in app_relative_bytecode_requests {
            if !app_relative.contains_key(&path) {
                app_relative.insert(path.clone(), AppRelativeResources::default());
            }

            let app_relative = app_relative.get_mut(&path).unwrap();

            for (name, request) in requests {
                let bytecode = match compiler.compile(
                    &request.source,
                    &name,
                    request.optimize_level,
                    // Bytecode in app-relative directories should never be mutated. So we
                    // shouldn't need to verify its hash at run-time.
                    // TODO consider making this configurable.
                    CompileMode::PycUncheckedHash,
                ) {
                    Ok(res) => res,
                    Err(msg) => panic!("error compiling bytecode for {}: {}", name, msg),
                };

                app_relative.module_bytecodes.insert(
                    name.clone(),
                    PackagedModuleBytecode {
                        bytecode,
                        is_package: request.is_package,
                    },
                );
            }
        }
    }

    let mut all_embedded_modules = BTreeSet::new();
    let mut annotated_package_names = BTreeSet::new();

    for (name, source) in &embedded_sources {
        all_embedded_modules.insert(name.clone());

        if source.is_package {
            annotated_package_names.insert(name.clone());
        }
    }
    for (name, bytecode) in &embedded_bytecodes {
        all_embedded_modules.insert(name.clone());

        if bytecode.is_package {
            annotated_package_names.insert(name.clone());
        }
    }

    for (name, extension) in &embedded_built_extension_modules {
        all_embedded_modules.insert(name.clone());

        if extension.is_package {
            annotated_package_names.insert(name.clone());
        }
    }

    let derived_package_names = packages_from_module_names(all_embedded_modules.iter().cloned());

    let mut all_embedded_package_names = annotated_package_names.clone();
    for package in derived_package_names {
        if !all_embedded_package_names.contains(&package) {
            warn!(
                logger,
                "package {} not initially detected as such; is package detection buggy?", package
            );
            all_embedded_package_names.insert(package);
        }
    }

    // Prune resource files that belong to packages that don't have a corresponding
    // Python module package, as they won't be loadable by our custom importer.
    let embedded_resources = embedded_resources
        .iter()
        .filter_map(|(package, values)| {
            if !all_embedded_package_names.contains(package) {
                warn!(
                    logger,
                    "package {} does not exist; excluding resources: {:?}",
                    package,
                    values.keys()
                );
                None
            } else {
                Some((package.clone(), values.clone()))
            }
        })
        .collect();

    PythonResources {
        embedded: EmbeddedPythonResources {
            module_sources: embedded_sources,
            module_bytecodes: embedded_bytecodes,
            all_modules: all_embedded_modules,
            all_packages: all_embedded_package_names,
            resources: embedded_resources,
            extension_modules: embedded_extension_modules,
            built_extension_modules: embedded_built_extension_modules,
        },
        app_relative,
        read_files,
        license_files_path,
    }
}

/// Install all app-relative files next to the generated binary.
fn install_app_relative(
    logger: &slog::Logger,
    context: &BuildContext,
    path: &str,
    app_relative: &AppRelativeResources,
) -> Result<(), String> {
    let dest_path = context.app_exe_path.parent().unwrap().join(path);

    create_dir_all(&dest_path).or_else(|_| Err("could not create app-relative path"))?;

    warn!(
        logger,
        "installing {} app-relative Python source modules to {}",
        app_relative.module_sources.len(),
        dest_path.display(),
    );

    for (module_name, module_source) in &app_relative.module_sources {
        // foo.bar -> foo/bar
        let mut module_path = dest_path.clone();
        module_path.extend(module_name.split('.'));

        // Packages need to get normalized to /__init__.py.
        if module_source.is_package {
            module_path.push("__init__");
        }

        module_path.set_file_name(format!(
            "{}.py",
            module_path.file_name().unwrap().to_string_lossy()
        ));

        info!(
            logger,
            "installing Python module {} to {}",
            module_name,
            module_path.display()
        );

        let parent_dir = module_path.parent().unwrap();
        create_dir_all(&parent_dir).or_else(|_| {
            Err(format!(
                "failed to create directory {}",
                parent_dir.display()
            ))
        })?;

        fs::write(&module_path, &module_source.source)
            .or_else(|_| Err(format!("failed to write {}", module_path.display())))?;
    }

    warn!(
        logger,
        "resolved {} app-relative Python bytecode modules in {}",
        app_relative.module_bytecodes.len(),
        path,
    );

    for (module_name, module_bytecode) in &app_relative.module_bytecodes {
        // foo.bar -> foo/bar
        let mut module_path = dest_path.clone();

        // .pyc files go into a __pycache__ directory next to the package.

        // __init__ is special.
        if module_bytecode.is_package {
            module_path.extend(module_name.split('.'));
            module_path.push("__pycache__");
            module_path.push("__init__");
        } else if module_name.contains('.') {
            let parts: Vec<&str> = module_name.split('.').collect();

            module_path.extend(parts[0..parts.len() - 1].to_vec());
            module_path.push("__pycache__");
            module_path.push(parts[parts.len() - 1].to_string());
        } else {
            module_path.push("__pycache__");
            module_path.push(module_name);
        }

        module_path.set_file_name(format!(
            // TODO determine string from Python distribution in use.
            "{}.cpython-37.pyc",
            module_path.file_name().unwrap().to_string_lossy()
        ));

        info!(
            logger,
            "installing Python module bytecode {} to {}",
            module_name,
            module_path.display()
        );

        let parent_dir = module_path.parent().unwrap();
        create_dir_all(&parent_dir).or_else(|_| {
            Err(format!(
                "failed to create directory {}",
                parent_dir.display()
            ))
        })?;

        fs::write(&module_path, &module_bytecode.bytecode)
            .or_else(|_| Err(format!("failed to write {}", module_path.display())))?;
    }

    let mut resource_count = 0;
    let mut resource_map = BTreeMap::new();
    for (package, entries) in &app_relative.resources {
        let mut names = BTreeSet::new();
        names.extend(entries.keys());
        resource_map.insert(package.clone(), names);
        resource_count += entries.len();
    }

    warn!(
        logger,
        "resolved {} app-relative resource files across {} packages",
        resource_count,
        app_relative.resources.len(),
    );

    for (package, entries) in &app_relative.resources {
        let package_path = dest_path.join(package);

        warn!(
            logger,
            "installing {} app-relative resource files to {}:{}",
            entries.len(),
            path,
            package,
        );

        for (name, data) in entries {
            let dest_path = package_path.join(name);

            info!(
                logger,
                "installing app-relative resource {}:{} to {}",
                package,
                name,
                dest_path.display()
            );

            create_dir_all(dest_path.parent().unwrap()).or_else(|e| Err(e.to_string()))?;

            fs::write(&dest_path, data)
                .or_else(|_| Err(format!("failed to write {}", dest_path.display())))?;
        }
    }

    Ok(())
}

/// Package a built Rust project into its packaging directory.
///
/// This will delete all content in the application's package directory.
pub fn package_project(logger: &slog::Logger, context: &mut BuildContext) -> Result<(), String> {
    warn!(
        logger,
        "packaging application into {}",
        context.app_path.display()
    );

    if context.app_path.exists() {
        warn!(logger, "purging {}", context.app_path.display());
        std::fs::remove_dir_all(&context.app_path).or_else(|e| Err(e.to_string()))?;
    }

    create_dir_all(&context.app_path).or_else(|e| Err(e.to_string()))?;

    warn!(
        logger,
        "copying {} to {}",
        context.app_exe_target_path.display(),
        context.app_exe_path.display()
    );
    std::fs::copy(&context.app_exe_target_path, &context.app_exe_path)
        .or_else(|_| Err("failed to copy built application"))?;

    warn!(logger, "resolving packaging state...");
    let state = context.get_packaging_state()?;

    if let Some(licenses_path) = state.license_files_path {
        let licenses_path = if licenses_path.is_empty() {
            context.app_path.clone()
        } else {
            context.app_path.join(licenses_path)
        };

        for (name, lis) in &state.license_infos {
            for li in lis {
                let path = licenses_path.join(&li.license_filename);
                warn!(logger, "writing license for {} to {}", name, path.display());
                fs::write(&path, li.license_text.as_bytes()).or_else(|e| Err(e.to_string()))?;
            }
        }
    }

    if !state.app_relative_resources.is_empty() {
        warn!(
            logger,
            "installing resources into {} app-relative directories",
            state.app_relative_resources.len(),
        );
    }

    for (path, v) in &state.app_relative_resources {
        install_app_relative(logger, context, path.as_str(), v).unwrap();
    }

    warn!(
        logger,
        "{} packaged into {}",
        context.app_name,
        context.app_path.display()
    );

    Ok(())
}

/// Defines files, etc to embed Python in a larger binary.
///
/// Instances are typically produced by processing a PyOxidizer config file.
#[derive(Debug)]
pub struct EmbeddedPythonConfig {
    /// Parsed starlark config.
    pub config: Config,

    /// Path to archive with source Python distribution.
    pub python_distribution_path: PathBuf,

    /// Path to frozen importlib._bootstrap bytecode.
    pub importlib_bootstrap_path: PathBuf,

    /// Path to frozen importlib._bootstrap_external bytecode.
    pub importlib_bootstrap_external_path: PathBuf,

    /// Path to file containing all known module names.
    pub module_names_path: PathBuf,

    /// Path to file containing packed Python module source data.
    pub py_modules_path: PathBuf,

    /// Path to file containing packed Python resources data.
    pub resources_path: PathBuf,

    /// Path to library file containing Python.
    pub libpython_path: PathBuf,

    /// Lines that can be emitted from Cargo build scripts to describe this
    /// configuration.
    pub cargo_metadata: Vec<String>,

    /// Rust source code to instantiate a PythonConfig instance using this config.
    pub python_config_rs: String,

    /// Path to file containing packaging state.
    pub packaging_state_path: PathBuf,
}

/// Derive build artifacts from a PyOxidizer configuration.
///
/// This function processes the PyOxidizer configuration and turns it into a set
/// of derived files that can power an embedded Python interpreter.
///
/// Returns a data structure describing the results.
#[allow(clippy::cognitive_complexity)]
pub fn process_config(
    logger: &slog::Logger,
    context: &mut BuildContext,
    opt_level: &str,
) -> EmbeddedPythonConfig {
    let mut cargo_metadata: Vec<String> = Vec::new();

    let config = &context.config;
    let dest_dir = &context.pyoxidizer_artifacts_path;

    warn!(
        logger,
        "processing config file {}",
        config.config_path.display()
    );

    cargo_metadata.push(format!(
        "cargo:rerun-if-changed={}",
        config.config_path.display()
    ));

    if !dest_dir.exists() {
        create_dir_all(dest_dir).unwrap();
    }

    if let PythonDistributionLocation::Local { local_path, .. } = &config.python_distribution {
        cargo_metadata.push(format!("cargo:rerun-if-changed={}", local_path));
    }

    // Obtain the configured Python distribution and parse it to a data structure.
    warn!(logger, "resolving Python distribution...");
    let python_distribution_path =
        resolve_python_distribution_archive(&config.python_distribution, &dest_dir);
    warn!(
        logger,
        "Python distribution available at {}",
        python_distribution_path.display()
    );

    let dist = ParsedPythonDistribution::from_path(
        logger,
        &python_distribution_path,
        &context.python_distribution_path,
    )
    .unwrap();

    warn!(logger, "distribution info: {:#?}", dist.as_minimal_info());

    // Produce the custom frozen importlib modules.
    warn!(
        logger,
        "compiling custom importlib modules to support in-memory importing"
    );
    let importlib = derive_importlib(&dist);

    let importlib_bootstrap_path = Path::new(&dest_dir).join("importlib_bootstrap");
    let mut fh = fs::File::create(&importlib_bootstrap_path).unwrap();
    fh.write_all(&importlib.bootstrap_bytecode).unwrap();

    let importlib_bootstrap_external_path =
        Path::new(&dest_dir).join("importlib_bootstrap_external");
    let mut fh = fs::File::create(&importlib_bootstrap_external_path).unwrap();
    fh.write_all(&importlib.bootstrap_external_bytecode)
        .unwrap();

    warn!(
        logger,
        "resolving Python resources (modules, extensions, resource data, etc)..."
    );
    let resources = resolve_python_resources(logger, context, &dist);

    warn!(
        logger,
        "resolved {} embedded Python source modules",
        resources.embedded.module_sources.len(),
    );
    info!(logger, "{:#?}", resources.embedded.module_sources.keys());
    warn!(
        logger,
        "resolved {} embedded Python bytecode modules",
        resources.embedded.module_bytecodes.len(),
    );
    info!(logger, "{:#?}", resources.embedded.module_bytecodes.keys());
    warn!(
        logger,
        "resolved {} unique embedded Python modules",
        resources.embedded.all_modules.len(),
    );
    info!(logger, "{:#?}", resources.embedded.all_modules);

    let mut resource_count = 0;
    let mut resource_map = BTreeMap::new();
    for (package, entries) in &resources.embedded.resources {
        let mut names = BTreeSet::new();
        names.extend(entries.keys());
        resource_map.insert(package.clone(), names);
        resource_count += entries.len();
    }

    warn!(
        logger,
        "resolved {} embedded resource files across {} packages",
        resource_count,
        resources.embedded.resources.len(),
    );
    info!(logger, "{:#?}", resource_map);

    let all_extension_modules = resources.embedded.embedded_extension_module_names();
    warn!(
        logger,
        "resolved {} embedded extension modules",
        all_extension_modules.len()
    );
    info!(logger, "{:#?}", all_extension_modules);

    // Produce the packed data structures containing Python modules.
    // TODO there is tons of room to customize this behavior, including
    // reordering modules so the memory order matches import order.

    warn!(logger, "writing packed Python module and resource data...");
    let module_names_path = Path::new(&dest_dir).join("py-module-names");
    let py_modules_path = Path::new(&dest_dir).join("py-modules");
    let resources_path = Path::new(&dest_dir).join("python-resources");

    let mut module_names_fh =
        BufWriter::new(fs::File::create(&module_names_path).expect("error creating file"));
    let mut modules_fh =
        BufWriter::new(fs::File::create(&py_modules_path).expect("error creating file"));
    let mut resources_fh =
        BufWriter::new(fs::File::create(&resources_path).expect("error creating file"));

    resources
        .embedded
        .write_blobs(&mut module_names_fh, &mut modules_fh, &mut resources_fh);

    module_names_fh.flush().unwrap();
    modules_fh.flush().unwrap();
    resources_fh.flush().unwrap();

    warn!(
        logger,
        "{} bytes of Python module data written to {}",
        py_modules_path.metadata().unwrap().len(),
        py_modules_path.display()
    );
    warn!(
        logger,
        "{} bytes of resources data written to {}",
        resources_path.metadata().unwrap().len(),
        resources_path.display()
    );

    // Produce a static library containing the Python bits we need.
    warn!(
        logger,
        "generating custom link library containing Python..."
    );
    let libpython_info = link_libpython(
        logger,
        &dist,
        &resources.embedded,
        dest_dir,
        &context.host_triple,
        &context.target_triple,
        opt_level,
    );
    cargo_metadata.extend(libpython_info.cargo_metadata);

    for p in &resources.read_files {
        cargo_metadata.push(format!("cargo:rerun-if-changed={}", p.display()));
    }

    warn!(logger, "processing python run mode: {:?}", config.run);
    warn!(
        logger,
        "processing embedded python config: {:?}", config.embedded_python_config
    );

    let python_config_rs = derive_python_config(
        &config.embedded_python_config,
        &config.run,
        &importlib_bootstrap_path,
        &importlib_bootstrap_external_path,
        &py_modules_path,
        &resources_path,
    );

    let dest_path = Path::new(&dest_dir).join("data.rs");
    write_data_rs(&dest_path, &python_config_rs);
    // Define the path to the written file in an environment variable so it can
    // be anywhere.
    cargo_metadata.push(format!(
        "cargo:rustc-env=PYEMBED_DATA_RS_PATH={}",
        dest_path.display()
    ));

    // Write a file containing the cargo metadata lines. This allows those
    // lines to be consumed elsewhere and re-emitted without going through all the
    // logic in this function.
    let cargo_metadata_path = Path::new(&dest_dir).join("cargo_metadata.txt");
    fs::write(&cargo_metadata_path, cargo_metadata.join("\n").as_bytes())
        .expect("unable to write cargo_metadata.txt");

    let packaging_state = PackagingState {
        license_files_path: resources.license_files_path,
        license_infos: libpython_info.license_infos,
        app_relative_resources: resources.app_relative,
    };

    let packaging_state_path = dest_dir.join("packaging_state.cbor");
    warn!(
        logger,
        "writing packaging state to {}",
        packaging_state_path.display()
    );
    let mut fh = BufWriter::new(
        fs::File::create(&packaging_state_path).expect("unable to create packaging_state.cbor"),
    );
    serde_cbor::to_writer(&mut fh, &packaging_state).unwrap();

    context.packaging_state = Some(packaging_state);

    EmbeddedPythonConfig {
        config: config.clone(),
        python_distribution_path,
        importlib_bootstrap_path,
        importlib_bootstrap_external_path,
        module_names_path,
        py_modules_path,
        resources_path,
        libpython_path: libpython_info.path,
        cargo_metadata,
        python_config_rs,
        packaging_state_path,
    }
}

/// Runs packaging/embedding from the context of a build script.
///
/// This function should be called by the build script for the package
/// that wishes to embed a Python interpreter/application. When called,
/// a PyOxidizer configuration file is found and read. The configuration
/// is then applied to the current build. This involves obtaining a
/// Python distribution to embed (possibly by downloading it from the Internet),
/// analyzing the contents of that distribution, extracting relevant files
/// from the distribution, compiling Python bytecode, and generating
/// resources required to build the ``pyembed`` crate/modules.
///
/// If everything works as planned, this whole process should be largely
/// invisible and the calling application will have an embedded Python
/// interpreter when it is built.
pub fn run_from_build(logger: &slog::Logger, build_script: &str) {
    // Adding our our rerun-if-changed lines will overwrite the default, so
    // we need to emit the build script name explicitly.
    println!("cargo:rerun-if-changed={}", build_script);

    println!("cargo:rerun-if-env-changed=PYOXIDIZER_CONFIG");

    let host = env::var("HOST").expect("HOST not defined");
    let target = env::var("TARGET").expect("TARGET not defined");
    let opt_level = env::var("OPT_LEVEL").expect("OPT_LEVEL not defined");
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not found");
    let profile = env::var("PROFILE").expect("PROFILE not defined");

    let project_path = PathBuf::from(&manifest_dir);

    let config_path = match find_pyoxidizer_config_file_env(logger, &PathBuf::from(manifest_dir)) {
        Some(v) => v,
        None => panic!("Could not find PyOxidizer config file"),
    };

    if !config_path.exists() {
        panic!("PyOxidizer config file does not exist");
    }

    let dest_dir = match env::var("PYOXIDIZER_ARTIFACT_DIR") {
        Ok(ref v) => PathBuf::from(v),
        Err(_) => PathBuf::from(env::var("OUT_DIR").unwrap()),
    };

    let mut context = BuildContext::new(
        logger,
        &project_path,
        &config_path,
        Some(&host),
        &target,
        profile == "release",
        // TODO Config value won't be honored here. Is that OK?
        Some(&dest_dir),
        true,
    )
    .unwrap();

    for line in process_config(logger, &mut context, &opt_level).cargo_metadata {
        println!("{}", line);
    }
}
