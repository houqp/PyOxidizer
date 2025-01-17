// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use starlark::environment::Environment;
use starlark::values::{
    default_compare, RuntimeError, TypedValue, Value, ValueError, ValueResult,
    INCORRECT_PARAMETER_TYPE_ERROR_CODE,
};
use starlark::{
    any, immutable, not_supported, starlark_fun, starlark_module, starlark_signature,
    starlark_signature_extraction, starlark_signatures,
};
use std::any::Any;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;

use super::distribution::{TarballDistribution, WixInstallerDistribution};
use super::embedded_python_config::EmbeddedPythonConfig;
use super::env::{required_str_arg, required_type_arg};
use super::python_distribution::PythonDistribution;
use super::python_packaging::{
    FilterInclude, Stdlib, StdlibExtensionVariant, StdlibExtensionsExplicitExcludes,
    StdlibExtensionsExplicitIncludes, StdlibExtensionsPolicy, WriteLicenseFiles,
};
use super::python_run_mode::PythonRunMode;
use crate::app_packaging::config::{
    BuildConfig as ConfigBuildConfig, Config as ConfigConfig, Distribution, PythonPackaging,
};
use crate::app_packaging::environment::EnvironmentContext;
use crate::py_packaging::config::{EmbeddedPythonConfig as ConfigEmbeddedPythonConfig, RunMode};
use crate::py_packaging::distribution::PythonDistributionLocation;

#[derive(Debug, Clone)]
pub struct Config {
    pub config: ConfigConfig,
}

impl TypedValue for Config {
    immutable!();
    any!();
    not_supported!(binop);
    not_supported!(container);
    not_supported!(function);
    not_supported!(get_hash);
    not_supported!(to_int);

    fn to_str(&self) -> String {
        format!("Config<{:#?}>", self.config)
    }

    fn to_repr(&self) -> String {
        self.to_str()
    }

    fn get_type(&self) -> &'static str {
        "Config"
    }

    fn to_bool(&self) -> bool {
        true
    }

    fn compare(&self, other: &dyn TypedValue, _recursion: u32) -> Result<Ordering, ValueError> {
        default_compare(self, other)
    }
}

starlark_module! { config_env =>
    #[allow(non_snake_case, clippy::ptr_arg)]
    Config(
        env env,
        application_name,
        embedded_python_config=None,
        python_distribution=None,
        packaging_rules=None,
        python_run_mode=None,
        distributions=None
    ) {
        let application_name = required_str_arg("application_name", &application_name)?;
        required_type_arg("embedded_python_config", "EmbeddedPythonConfig", &embedded_python_config)?;
        required_type_arg("python_distribution", "PythonDistribution", &python_distribution)?;
        required_type_arg("python_run_mode", "PythonRunMode", &python_run_mode)?;

        let context = env.get("CONTEXT").expect("CONTEXT not set");

        let build_path = context.downcast_apply(|x: &EnvironmentContext| x.build_path.clone());

        let build_config = ConfigBuildConfig {
            application_name,
            build_path,
        };

        let embedded_python_config = embedded_python_config.downcast_apply(|x: &EmbeddedPythonConfig| -> ConfigEmbeddedPythonConfig {
            x.config.clone()
        });
        let python_distribution = python_distribution.downcast_apply(|x: &PythonDistribution| -> PythonDistributionLocation {
            x.source.clone()
        });

        let python_packaging: Vec<Result<PythonPackaging, ValueError>> = packaging_rules.into_iter()?.map(|x| -> Result<PythonPackaging, ValueError> {
            match x.get_type() {
                "FilterInclude" => Ok(x.downcast_apply(|x: &FilterInclude| -> PythonPackaging {
                    PythonPackaging::FilterInclude(x.rule.clone())
                })),
                "Stdlib" => Ok(x.downcast_apply(|x: &Stdlib| -> PythonPackaging {
                    PythonPackaging::Stdlib(x.rule.clone())
                })),
                "StdlibExtensionVariant" => Ok(x.downcast_apply(|x: &StdlibExtensionVariant| -> PythonPackaging {
                    PythonPackaging::StdlibExtensionVariant(x.rule.clone())
                })),
                "StdlibExtensionsExplicitExcludes" => Ok(x.downcast_apply(|x: &StdlibExtensionsExplicitExcludes| -> PythonPackaging {
                    PythonPackaging::StdlibExtensionsExplicitExcludes(x.rule.clone())
                })),
                "StdlibExtensionsExplicitIncludes" => Ok(x.downcast_apply(|x: &StdlibExtensionsExplicitIncludes| -> PythonPackaging {
                    PythonPackaging::StdlibExtensionsExplicitIncludes(x.rule.clone())
                })),
                "StdlibExtensionsPolicy" => Ok(x.downcast_apply(|x: &StdlibExtensionsPolicy| -> PythonPackaging {
                    PythonPackaging::StdlibExtensionsPolicy(x.rule.clone())
                })),
                "WriteLicenseFiles" => Ok(x.downcast_apply(|x: &WriteLicenseFiles| -> PythonPackaging {
                    PythonPackaging::WriteLicenseFiles(x.rule.clone())
                })),
                t => Err(RuntimeError {
                    code: INCORRECT_PARAMETER_TYPE_ERROR_CODE,
                    message: format!("invalid packaging rule type: {}", t),
                    label: format!("invalid packaging rule type: {}", t),
                }.into()),
            }
        // This code is horrible but I couldn't figure out how to get the typing to work right.
        }).collect();
        for r in &python_packaging {
            if r.is_err() {
                return Err(r.clone().unwrap_err());
            }
        }
        let python_packaging = python_packaging.iter().map(|x| x.clone().unwrap()).collect();

        let run = python_run_mode.downcast_apply(|x: &PythonRunMode| -> RunMode {
            x.run_mode.clone()
        });

        let distributions = match distributions.get_type() {
            "list" => {
                let temp: Vec<Result<Distribution, RuntimeError>> = distributions.into_iter()?.map(|x| {
                    match x.get_type() {
                        "TarballDistribution" => Ok(x.downcast_apply(|x: &TarballDistribution| -> Distribution {
                            Distribution::Tarball(x.distribution.clone())
                        })),
                        "WixInstallerDistribution" => Ok(x.downcast_apply(|x: &WixInstallerDistribution| -> Distribution {
                            Distribution::WixInstaller(x.distribution.clone())
                        })),
                        t => Err(RuntimeError {
                            code: INCORRECT_PARAMETER_TYPE_ERROR_CODE,
                            message: format!("invalid packaging rule type: {}", t),
                            label: format!("invalid packaging rule type: {}", t),
                        }),
                    }
                }).collect();

                for r in &temp {
                    if r.is_err() {
                        return Err(r.clone().unwrap_err().into());
                    }
                }

                temp.iter().map(|x| x.clone().unwrap()).collect()
            },
            "NoneType" => Vec::new(),
            _ => return Err(RuntimeError {
                code: INCORRECT_PARAMETER_TYPE_ERROR_CODE,
                message: "distributions must be a list or None".to_string(),
                label: "distributions must be a list or None".to_string(),
            }.into())

        };

        let config_path = env.get("CONFIG_PATH").expect("CONFIG_PATH should always be available").to_string();

        let mut have_stdlib = false;
        let mut have_stdlib_extensions_policy = false;

        for packaging in &python_packaging {
            match packaging {
                &PythonPackaging::Stdlib(_) => have_stdlib = true,
                &PythonPackaging::StdlibExtensionsPolicy(_) => have_stdlib_extensions_policy = true,
                _ => ()
            }
        }

        if !have_stdlib_extensions_policy {
            return Err(RuntimeError {
                code: INCORRECT_PARAMETER_TYPE_ERROR_CODE,
                message: "no StdLibExtensionsPolicy packaging rule".to_string(),
                label: "no StdLibExtensionsPolicy packaging rule".to_string(),
            }.into());
        }

        if !have_stdlib {
            return Err(RuntimeError {
                code: INCORRECT_PARAMETER_TYPE_ERROR_CODE,
                message: "no StdLib packaging rule".to_string(),
                label: "no StdLib packaging rule".to_string(),
            }.into());
        }

        let config = ConfigConfig {
            config_path: PathBuf::from(config_path),
            build_config,
            embedded_python_config,
            python_distribution,
            python_packaging,
            run,
            distributions,
        };

        let v = Value::new(Config { config });

        env.get_parent().unwrap().set("CONFIG", v.clone()).unwrap();

        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use indoc::indoc;

    #[test]
    fn test_config_default() {
        let err = starlark_nok("Config()");
        assert!(err
            .message
            .starts_with("Missing parameter application_name"));
    }

    #[test]
    fn test_config_basic() {
        let content = indoc!(
            r#"
            Config(
                application_name='myapp',
                embedded_python_config=EmbeddedPythonConfig(),
                python_distribution=default_python_distribution(),
                python_run_mode=python_run_mode_repl(),
                packaging_rules=[Stdlib(), StdlibExtensionsPolicy('minimal')],
            )
        "#
        );

        let v = starlark_ok(content);
        assert_eq!(v.get_type(), "Config");
    }
}
