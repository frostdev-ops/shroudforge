// SPDX-License-Identifier: MIT
//! Kiln layer: an `AppState` over an empty fixture game, for tests outside this module (`game.types`).

use super::*;

impl AppState {
    /// An `AppState` with `type_registry` (pregame, server, no asset transaction) over an empty game in a new temporary
    /// folder, which the caller removes once the state is dropped.
    pub(crate) fn for_tests(type_registry: Rc<TypeRegistry>) -> (std::path::PathBuf, Self) {
        let root = std::env::temp_dir().join(format!("sf-kiln-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("mods")).unwrap();
        for extension in ["kfc", "kfc_resources"] {
            std::fs::write(root.join(format!("game.{extension}")), []).unwrap();
        }
        KFCWriter::<Rc<KFCFile>, _>::new(
            &root,
            "game",
            Rc::new(TypeRegistry::default()),
            "fixture",
        )
        .unwrap()
        .finalize()
        .unwrap();
        let root_utf8 = PathBuf::from_path_buf(root.clone()).unwrap();
        let env = ModEnvironment::load(&root_utf8).unwrap();
        let ref_file = crate::game_files::load_kfc_file(root_utf8.join("game.kfc")).unwrap();
        let reader =
            crate::game_files::create_reader_with_extension(&root_utf8, "game", "kfc").unwrap();
        let api = ShroudForgeApi::new(
            shroudforge_compatibility::Compatibility::new(Vec::<String>::new())
                .resolve(kfc::schema_from_registry(&type_registry, &ref_file)),
        );
        let state = AppState {
            runtime_configured: Cell::new(false),
            runtime_active_mods: RefCell::default(),
            runtime_effects: RefCell::default(),
            runtime_write_fault: RefCell::default(),
            runtime_ownership: Rc::new(RefCell::default()),
            env,
            api,
            config: AppConfig {
                is_server: true,
                phase: RuntimePhase::Pregame,
                feature_flags: AppFeatures::empty(),
                export_dir: root_utf8.join("exports"),
                export_touches: None,
            },
            type_registry,
            attribute_catalog: OnceCell::new(),
            inline_fields: RefCell::default(),
            inline_field_bytes: Cell::new(0),
            ref_file,
            reader: RefCell::new(Some(reader)),
            writer: RefCell::new(None),
            asset_transaction: Cell::new(false),
            asset_stem: "game".to_owned(),
            new_contents: RefCell::default(),
            content_reads_in_flight: Arc::new(AtomicUsize::new(0)),
            resources: RefCell::default(),
            loaded_resource_indexes: RefCell::default(),
            types: RefCell::default(),
        };
        (root, state)
    }
}
