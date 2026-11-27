use std::rc::Rc;

use mod_loader::{Capability, Mod, ModManifest};
use tracing::debug;

use crate::{
    env::{AppFeatures, AppState},
    lua::{FunctionArgs, LuaVM},
    runner::state::{RunnerLocalState, RunnerState},
};

pub(crate) use super::require_mod as state;

pub struct LuaModRunner {
    pub(crate) lua: LuaVM,
    state: Rc<RunnerState>,
}

impl LuaModRunner {
    /// The job has its own globals while retaining the real mod's bound APIs.
    pub(crate) fn diagnostic_environment(&self, mod_id: &str) -> mlua::Result<mlua::Table> {
        let source = self.state.environment(mod_id).ok_or_else(|| {
            mlua::Error::runtime(format!("diagnostic API owner is not loaded: {mod_id}"))
        })?;
        let env = self.lua.create_table()?;
        for entry in source.pairs::<mlua::Value, mlua::Value>() {
            let (key, value) = entry?;
            if key
                .as_string()
                .is_some_and(|name| name.as_bytes().as_ref() == b"_G")
            {
                continue;
            }
            env.raw_set(key, value)?;
        }
        env.raw_set("_G", env.clone())?;
        Ok(env)
    }

    pub fn new(context: AppState) -> mlua::Result<Self> {
        let lua = LuaVM::new()?;
        let state = Rc::new(RunnerState::default());

        lua.set_app_data(context);

        Ok(Self { lua, state })
    }

    /// Kiln layer: an error names the package whose entrypoint raised it (`PregameModError`), so the
    /// loader can run the pass once more without that package. Its text is the Lua error's, as before.
    /// A panic in an entrypoint goes on, naming the package (`PregameModPanic`).
    pub fn run(&self) -> Result<(), crate::PregameModError> {
        let app_state = self.lua.app_data_ref::<AppState>().unwrap();

        for r#mod in self.state.mods().values() {
            // Runtime mods may export files during play. Export permission does
            // not make their entrypoints safe to execute during asset preparation.
            if !r#mod.info().requires_pregame() {
                continue;
            }
            let has_patch_capability = r#mod.info().capabilities.contains(&Capability::Patch);
            let has_export_capability = r#mod.info().capabilities.contains(&Capability::Export);
            let should_run = has_patch_capability && app_state.has_feature(AppFeatures::PATCH)
                || has_export_capability && app_state.has_feature(AppFeatures::EXPORT);

            if !should_run {
                continue;
            }

            debug!(
                mod_id = r#mod.info().id,
                mod_name = r#mod.info().name,
                "Running mod",
            );

            // a Rust panic in it goes on to the loader with the package's name (`PregameModPanic`)
            let info = r#mod.info();
            // Kiln layer: what the pass is asked to change while this entrypoint runs is this package's
            let journal = app_state.change_journal().cloned();
            if let Some(journal) = &journal {
                journal.begin(&info.id, Some(info.version.to_string()));
            }
            let ran = crate::PregameModPanic::name_package(&info.id, &info.name, || {
                self.state.require_mod(&info.id, &self.lua)
            });
            if let Some(journal) = &journal {
                let registry = app_state.type_registry().clone();
                journal.end(
                    self.journal_snapshots(&app_state),
                    |id| app_state.reader().ok()?.read_resource(id).ok().flatten(),
                    |id, bytes| {
                        let r#type = kfc::kfc_format::type_for_resource(&registry, id)?;
                        kfc::resource::value::Value::from_bytes(&registry, r#type, bytes).ok()
                    },
                );
            }
            ran.map_err(|error| crate::PregameModError::new(&info.id, error))?;
        }

        Ok(())
    }

    /// Kiln layer: every resource the pass edited so far (a record whose value is replaced or written), as the commit
    /// would encode it now (`ResourceInfo::apply`, no Lua runs), for the change journal. A record that cannot be encoded is
    /// left out: the commit fails on it too.
    pub(crate) fn journal_snapshots(&self, app_state: &AppState) -> Vec<crate::change_journal::Snapshot> {
        let registry = app_state.type_registry().clone();
        app_state
            .get_cached_resources()
            .into_iter()
            .filter(|r| r.source_name() != "loaded-runtime")
            .filter_map(|r| {
                let value = r.apply(&self.lua).ok()??;
                let r#type = kfc::kfc_format::type_for_resource(&registry, &r.resource_id)?;
                let mut bytes = Vec::new();
                value.write_into(&registry, r#type, &mut bytes).ok()?;
                Some(crate::change_journal::Snapshot {
                    id: r.resource_id,
                    now: Some((value, bytes)),
                    created: r.source_name() == "asset-transaction",
                })
            })
            .collect()
    }

    pub(crate) fn runtime_mod_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for r#mod in self.state.mods().values() {
            if !r#mod
                .info()
                .capabilities
                .iter()
                .any(|capability| capability.requires_runtime())
            {
                continue;
            }
            ids.push(r#mod.info().id.clone());
        }
        ids
    }

    pub(crate) fn load_runtime_module(&self, id: &str) -> mlua::Result<crate::lua::LuaValue> {
        self.state.require_mod(id, &self.lua)
    }

    pub fn setup<'a>(&self, mods: impl IntoIterator<Item = &'a Mod>) -> mlua::Result<()> {
        for r#mod in mods {
            tracing::debug!(target: "shroudforge::runtime", stage = "runner-setup",
                mod_id = %r#mod.info().id, "Preparing runtime mod environment");
            self.setup_mod(r#mod)?;
            tracing::debug!(target: "shroudforge::runtime", stage = "runner-setup",
                mod_id = %r#mod.info().id, "Runtime mod environment ready");
        }

        Ok(())
    }

    fn setup_mod(&self, r#mod: &Mod) -> mlua::Result<()> {
        let env = self.create_mod_environment(r#mod.info())?;

        crate::env::register(&self.lua, &env, r#mod)?;

        self.state.add_mod(r#mod.clone(), env);

        Ok(())
    }

    fn create_mod_environment(&self, mod_info: &ModManifest) -> mlua::Result<mlua::Table> {
        let env = self.lua.create_base_environment()?;
        let local_state = RunnerLocalState::new(&mod_info.id, self.state.clone());

        env.raw_set(
            "require",
            self.lua.create_function(move |lua, args: FunctionArgs| {
                let id = args.get::<String>(0)?;

                local_state.require(&id, lua)
            })?,
        )?;

        Ok(env)
    }
}
