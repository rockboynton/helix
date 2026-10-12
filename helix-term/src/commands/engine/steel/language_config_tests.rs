use super::{Config, HelixConfiguration};
use arc_swap::ArcSwap;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use steel::{
    rvals::SteelString,
    steel_vm::{builtin::BuiltInModule, engine::Engine, register_fn::RegisterFn},
    SteelVal,
};

fn comparable(mut value: serde_json::Value) -> serde_json::Value {
    match &mut value {
        serde_json::Value::Object(fields) => {
            for (key, field) in fields {
                *field = comparable(field.take());
                // These are HashSets; iteration order is not part of the config.
                if key == "only-features" || key == "except-features" {
                    if let Some(values) = field.as_array_mut() {
                        values.sort_by_key(|value| value.to_string());
                    }
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                *value = comparable(value.take());
            }
        }
        _ => {}
    }
    value
}

#[test]
fn personal_languages_match_toml_overrides_and_preserve_defaults() {
    let base = helix_loader::config::default_lang_config();
    let loader = helix_core::syntax::Loader::new(base.clone().try_into().unwrap()).unwrap();
    let config = Arc::new(Mutex::new(HelixConfiguration {
        configuration: Arc::new(ArcSwap::from_pointee(Config::default())),
        language_configuration: Arc::new(ArcSwap::from_pointee(loader)),
    }));
    let refreshes = Arc::new(AtomicUsize::new(0));
    let mut module = BuiltInModule::new("helix/core/configuration");
    let setter = config.clone();
    module.register_fn(
        "set-lsp-config!",
        move |name: SteelString, values: HashMap<String, SteelVal>| {
            setter
                .lock()
                .unwrap()
                .update_language_server_config(name, values)
        },
    );
    let getter = config.clone();
    module.register_fn("get-lsp-config", move |name: SteelString| {
        getter.lock().unwrap().get_language_server_config(name)
    });
    let languages = config.clone();
    module.register_fn(
        "update-language-config!",
        move |name: SteelString, values: SteelVal| {
            languages
                .lock()
                .unwrap()
                .update_language_config(name, values)
        },
    );
    let refreshed = refreshes.clone();
    module.register_fn("refresh-all-language-configs!", move || {
        refreshed.fetch_add(1, Ordering::Relaxed);
    });
    let mut engine = Engine::new();
    engine.register_module(module);
    engine.register_steel_module(
        "helix/configuration.scm".into(),
        r#"
        (require-builtin helix/core/configuration as native.)
        (provide set-lsp-config! get-lsp-config refresh-all-language-configs!)
        (define set-lsp-config! native.set-lsp-config!)
        (define get-lsp-config native.get-lsp-config)
        (define refresh-all-language-configs! native.refresh-all-language-configs!)
    "#
        .into(),
    );
    engine
        .run(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../personal-config/languages.scm"
        )))
        .unwrap();
    assert_eq!(
        refreshes.load(Ordering::Relaxed),
        0,
        "Importing must not apply settings"
    );
    assert!(config
        .lock()
        .unwrap()
        .get_language_server_config("steel-language-server".into())
        .is_none());
    engine.run("(configure-languages!)").unwrap();
    assert_eq!(refreshes.load(Ordering::Relaxed), 1);

    let expected: toml::Value = toml::from_str(include_str!("languages-baseline.toml")).unwrap();
    let expected = helix_loader::merge_toml_values(base, expected, 3);
    let expected = helix_core::syntax::Loader::new(expected.try_into().unwrap()).unwrap();
    let config = config.lock().unwrap();
    let actual = config.language_configuration.load();
    // Compare every language and server, not just the explicitly overridden fields.
    for language in expected.language_configs() {
        let current = actual
            .language_configs()
            .find(|value| value.language_id == language.language_id)
            .unwrap();
        assert_eq!(
            comparable(serde_json::to_value(current).unwrap()),
            comparable(serde_json::to_value(language).unwrap()),
            "{}",
            language.language_id
        );
    }
    for (name, server) in expected.language_server_configs() {
        assert_eq!(
            serde_json::to_value(actual.language_server_configs().get(name).unwrap()).unwrap(),
            serde_json::to_value(server).unwrap(),
            "{name}"
        );
    }
}
