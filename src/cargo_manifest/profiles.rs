// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use cargo_toml::DebugSetting;
use cargo_toml::LtoSetting;
use cargo_toml::Profile;
use cargo_toml::Profiles;
use cargo_toml::StripSetting;
use toml_edit::Item;
use toml_edit::Table;

use super::toml_util::cargo_toml_to_toml_edit_value;
use super::toml_util::decorated_value;
use super::toml_util::maybe_add_to_table;
use super::toml_util::new_implicit_table;

/// Format profiles according to
/// https://doc.rust-lang.org/cargo/reference/profiles.html
pub fn profiles_to_toml(profiles: &Profiles) -> Table {
    let Profiles {
        release,
        dev,
        test,
        bench,
        doc,
        custom,
    } = profiles;

    let mut table = new_implicit_table();
    if let Some(p) = release {
        table["release"] = Item::Table(profile_to_toml(p));
    }
    if let Some(p) = dev {
        table["dev"] = Item::Table(profile_to_toml(p));
    }
    if let Some(p) = test {
        table["test"] = Item::Table(profile_to_toml(p));
    }
    if let Some(p) = bench {
        table["bench"] = Item::Table(profile_to_toml(p));
    }
    if let Some(p) = doc {
        table["doc"] = Item::Table(profile_to_toml(p));
    }
    for (name, p) in custom {
        table[name] = Item::Table(profile_to_toml(p));
    }
    table
}

fn profile_to_toml(profile: &Profile) -> Table {
    let Profile {
        opt_level,
        debug,
        split_debuginfo,
        rpath,
        lto,
        debug_assertions,
        codegen_units,
        panic,
        incremental,
        overflow_checks,
        strip,
        package: _,
        build_override: _,
        inherits: _,
    } = profile;

    let mut table = new_implicit_table();
    {
        let table = &mut table;
        if let Some(v) = opt_level {
            table["opt-level"] = decorated_value(cargo_toml_to_toml_edit_value(v));
        }
        if let Some(v) = debug {
            table["debug"] = match v {
                DebugSetting::None => decorated_value(false),
                DebugSetting::Lines => decorated_value(1),
                DebugSetting::Full => decorated_value(true),
            };
        }
        maybe_add_to_table(table, "split-debuginfo", split_debuginfo.as_deref());
        maybe_add_to_table(table, "rpath", *rpath);
        if let Some(v) = lto {
            table["lto"] = match v {
                LtoSetting::None => decorated_value("off"),
                LtoSetting::ThinLocal => decorated_value(false),
                LtoSetting::Thin => decorated_value("thin"),
                LtoSetting::Fat => decorated_value(true),
            }
        }
        maybe_add_to_table(table, "debug-assertions", *debug_assertions);
        maybe_add_to_table(table, "codegen-units", (*codegen_units).map(i64::from));
        maybe_add_to_table(table, "panic", panic.as_deref());
        maybe_add_to_table(table, "incremental", *incremental);
        maybe_add_to_table(table, "overflow-checks", *overflow_checks);
        if let Some(v) = strip {
            table["strip"] = match v {
                StripSetting::None => decorated_value(false),
                StripSetting::Debuginfo => decorated_value("debuginfo"),
                StripSetting::Symbols => decorated_value(true),
            };
        }
    }
    table
}

#[cfg(test)]
pub fn empty_profile() -> Profile {
    Profile {
        opt_level: None,
        debug: None,
        split_debuginfo: None,
        rpath: None,
        lto: None,
        debug_assertions: None,
        codegen_units: None,
        panic: None,
        incremental: None,
        overflow_checks: None,
        strip: None,
        package: std::collections::BTreeMap::new(),
        build_override: None,
        inherits: None,
    }
}

#[cfg(test)]
mod test {
    use std::collections::BTreeMap;

    use cargo_toml::Value;

    use super::*;

    fn s(s: &str) -> String {
        s.to_owned()
    }

    #[test]
    fn profiles_to_toml_test_empty() {
        profiles_to_toml(&Profiles::default()).is_empty();
    }

    #[test]
    fn profiles_to_toml_test() {
        let table = profiles_to_toml(&Profiles {
            release: Some({
                Profile {
                    opt_level: Some(Value::String(s("opt-release"))),
                    ..empty_profile()
                }
            }),
            dev: Some({
                Profile {
                    opt_level: Some(Value::String(s("opt-dev"))),
                    ..empty_profile()
                }
            }),
            test: Some({
                Profile {
                    opt_level: Some(Value::String(s("opt-test"))),
                    ..empty_profile()
                }
            }),
            bench: Some({
                Profile {
                    opt_level: Some(Value::String(s("opt-bench"))),
                    ..empty_profile()
                }
            }),
            doc: Some({
                Profile {
                    opt_level: Some(Value::String(s("opt-doc"))),
                    ..empty_profile()
                }
            }),
            custom: BTreeMap::new(),
        });
        assert_eq!(
            toml_edit::Document::from(table).to_string(),
            r#"[release]
opt-level = "opt-release"

[dev]
opt-level = "opt-dev"

[test]
opt-level = "opt-test"

[bench]
opt-level = "opt-bench"

[doc]
opt-level = "opt-doc"
"#
        );
    }

    #[test]
    fn profile_to_toml_test_empty() {
        assert!(profile_to_toml(&empty_profile()).is_empty());
    }

    #[test]
    fn profile_to_toml_test() {
        assert_eq!(
            profile_to_toml(&Profile {
                opt_level: Some(Value::String(s("opt"))),
                debug: Some(DebugSetting::Full),
                split_debuginfo: None,
                rpath: Some(true),
                lto: Some(LtoSetting::Thin),
                debug_assertions: Some(false),
                codegen_units: Some(7u16),
                panic: Some(s("panic")),
                incremental: Some(true),
                overflow_checks: Some(false),
                strip: None,
                package: std::collections::BTreeMap::new(),
                build_override: None,
                inherits: None,
            })
            .to_string(),
            r#"opt-level = "opt"
debug = true
rpath = true
lto = "thin"
debug-assertions = false
codegen-units = 7
panic = "panic"
incremental = true
overflow-checks = false
"#
        );
    }
}
