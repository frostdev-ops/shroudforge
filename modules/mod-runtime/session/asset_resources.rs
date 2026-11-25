use super::*;
use crate::lua::StoredLuaValue;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub(crate) const MAX_CONTENT_READ_JOBS: usize = 4;
pub(crate) const MAX_CONTENT_READ_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct ContentReadPermit {
    _inner: Arc<ContentReadPermitInner>,
}

struct ContentReadPermitInner {
    in_flight: Arc<AtomicUsize>,
}

impl Drop for ContentReadPermitInner {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::Release);
    }
}

pub struct ResourceInfo {
    pub resource_id: ResourceId,
    pub(super) source: ResourceSource,
    pub(super) creation_session: u64,
    pub(super) runtime_snapshot: OnceCell<Rc<[u8]>>,
    pub(super) runtime_write_fault: RefCell<Option<String>>,

    pub(super) original_value: OnceCell<Option<MappedValue>>,
    // Kiln layer: in Lua's registry, not as an mlua value (lua/stored_lua_value.rs)
    pub(super) value: RefCell<Option<StoredLuaValue>>,
    pub(super) is_dirty: Cell<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ResourceSource {
    KfcFile,
    AssetTransaction,
    LoadedRuntime,
}

impl ResourceInfo {
    pub fn source_name(&self) -> &'static str {
        match self.source {
            ResourceSource::KfcFile => "kfc-file",
            ResourceSource::AssetTransaction => "asset-transaction",
            ResourceSource::LoadedRuntime => "loaded-runtime",
        }
    }

    pub fn get_lua_value(&self, lua: &mlua::Lua) -> mlua::Result<LuaValue> {
        if let Some(value) = self.value.borrow().as_ref() {
            return value.load();
        }

        let value = match self.get_mapped_value(lua)? {
            Some(value) => convert_value_to_lua(value, lua)?,
            None => LuaValue::Nil,
        };

        self.value
            .replace(Some(StoredLuaValue::new(lua, value.clone())?));

        Ok(value)
    }

    pub fn set_lua_value(&self, lua_value: LuaValue, lua: &mlua::Lua) -> mlua::Result<()> {
        if self.source == ResourceSource::LoadedRuntime {
            return Err(LuaError::generic("loaded runtime resources are read-only"));
        }
        let app_state = lua.app_data_ref::<AppState>().unwrap();
        if !app_state.has_feature(AppFeatures::PATCH) {
            return Err(LuaError::generic("game.assets.write is unavailable"));
        }
        let type_registry = app_state.type_registry();

        let r#type = kfc_format::type_for_resource(type_registry, &self.resource_id)
            .ok_or_else(|| LuaError::generic("resource type not found in current game registry"))?;

        let lua_value = validate_and_clone_lua_value(
            &lua_value,
            &TypeHandle::new(type_registry.clone(), r#type.index),
            lua,
        )
        .map_err(LuaError::external)?;

        self.value
            .replace(Some(StoredLuaValue::new(lua, lua_value)?));
        self.is_dirty.replace(true);

        Ok(())
    }

    pub fn apply(&self, lua: &mlua::Lua) -> mlua::Result<Option<Value>> {
        if self.source == ResourceSource::LoadedRuntime {
            return Ok(None);
        }
        let app_state = lua.app_data_ref::<AppState>().unwrap();
        let type_registry = app_state.type_registry();

        let lua_value = match self.value.borrow().as_ref() {
            Some(value) => value.load()?,
            None => return Ok(None),
        };
        let lua_value = &lua_value;

        if !self.is_dirty.get() && !is_dirty_lua_value(lua_value)? {
            return Ok(None);
        }

        let r#type = kfc_format::type_for_resource(type_registry, &self.resource_id)
            .ok_or_else(|| LuaError::generic("resource type not found in current game registry"))?;

        let value = convert_lua_to_value(
            lua_value,
            &TypeHandle::new(type_registry.clone(), r#type.index),
        )
        .map_err(LuaError::external)?;

        Ok(Some(value))
    }

    fn get_mapped_value(&self, lua: &mlua::Lua) -> mlua::Result<Option<&MappedValue>> {
        let app_state = lua.app_data_ref::<AppState>().unwrap();

        self.original_value
            .get_or_try_init(|| {
                let bytes = match self.source {
                    ResourceSource::KfcFile => app_state
                        .reader()?
                        .read_resource(&self.resource_id)?
                        .map(|bytes| Rc::<[u8]>::from(bytes)),
                    ResourceSource::AssetTransaction => None,
                    ResourceSource::LoadedRuntime => Some(
                        self.loaded_snapshot_bytes()
                            .map_err(LuaError::generic)?
                            .clone(),
                    ),
                };
                match bytes {
                    Some(value) => {
                        let type_registry = app_state.type_registry();
                        let r#type =
                            kfc_format::type_for_resource(type_registry, &self.resource_id)
                                .ok_or_else(|| {
                                    LuaError::generic(
                                        "resource type not found in current game registry",
                                    )
                                })?;

                        let value = MappedValue::from_bytes(type_registry, r#type, &value)
                            .map_err(LuaError::external)?;

                        Ok(Some(value))
                    }
                    None => Ok(None),
                }
            })
            .map(|v| v.as_ref())
    }
}

impl AppState {
    pub(crate) fn reserve_content_read(&self) -> Option<ContentReadPermit> {
        self.content_reads_in_flight
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |in_flight| {
                (in_flight < MAX_CONTENT_READ_JOBS).then_some(in_flight + 1)
            })
            .ok()?;

        Some(ContentReadPermit {
            _inner: Arc::new(ContentReadPermitInner {
                in_flight: Arc::clone(&self.content_reads_in_flight),
            }),
        })
    }

    #[inline]
    pub fn kfc_file(&self) -> &KFCFile {
        &self.ref_file
    }

    #[inline]
    pub fn reader(&self) -> mlua::Result<RefMut<'_, KFCCursor<KFCReader>>> {
        RefMut::filter_map(self.reader.borrow_mut(), Option::as_mut)
            .map_err(|_| LuaError::generic(super::GAME_FILE_HANDLES_RELEASED))
    }

    #[inline]
    pub fn take_writer(&self) -> mlua::Result<KFCWriter<Rc<KFCFile>, Rc<TypeRegistry>>> {
        self.writer
            .borrow_mut()
            .take()
            .ok_or_else(|| LuaError::generic("typed asset writer is unavailable in this phase"))
    }

    pub fn commit_assets(&self) -> mlua::Result<()> {
        if !self.asset_transaction.get() {
            return Err(LuaError::generic("typed asset transaction is unavailable"));
        }
        // Publishing replaces enshrouded.kfc_resources, which the reader keeps
        // open. Windows replaces open files with POSIX semantics, Wine/Proton
        // rejects that with "Access denied (os error 5)".
        self.reader.borrow_mut().take();
        shroudforge_game_assets::transaction::commit(
            self.env.game_dir().as_std_path(),
            &self.asset_stem,
        )
        .map_err(LuaError::external)?;
        self.asset_transaction.set(false);
        Ok(())
    }

    pub fn get_cached_resources(&self) -> Vec<Rc<ResourceInfo>> {
        self.resources.borrow().values().cloned().collect()
    }

    pub fn get_resource_info(&self, guid: &ResourceId) -> Option<Rc<ResourceInfo>> {
        match self.resources.borrow_mut().entry(*guid) {
            Entry::Occupied(entry) => Some(entry.get().clone()),
            Entry::Vacant(entry) => {
                if !self.ref_file.resources().contains_key(guid) {
                    return None;
                }

                let info = ResourceInfo {
                    resource_id: *guid,
                    source: ResourceSource::KfcFile,
                    creation_session: 0,
                    runtime_snapshot: OnceCell::new(),
                    runtime_write_fault: RefCell::new(None),
                    original_value: OnceCell::new(),
                    value: RefCell::default(),
                    is_dirty: Cell::new(false),
                };
                let info = Rc::new(info);

                entry.insert(info.clone());

                Some(info)
            }
        }
    }

    pub fn add_resource(
        &self,
        value: &LuaValue,
        guid: &ResourceId,
        lua: &mlua::Lua,
    ) -> mlua::Result<()> {
        if !self.has_feature(AppFeatures::PATCH) {
            return Err(LuaError::generic("game.assets.write is unavailable"));
        }
        let mut resources = self.resources.borrow_mut();

        if resources.contains_key(guid) {
            return Err(LuaError::generic(format!(
                "resource with GUID {guid} already exists"
            )));
        }

        let r#type = TypeHandle::new(
            self.type_registry.clone(),
            kfc_format::type_for_resource(&self.type_registry, guid)
                .ok_or_else(|| {
                    LuaError::generic("resource type not found in current game registry")
                })?
                .index,
        );
        let value =
            validate_and_clone_lua_value(value, &r#type, lua).map_err(LuaError::external)?;

        let info = Rc::new(ResourceInfo {
            resource_id: *guid,
            source: ResourceSource::AssetTransaction,
            creation_session: 0,
            runtime_snapshot: OnceCell::new(),
            runtime_write_fault: RefCell::new(None),
            original_value: OnceCell::new(),
            value: RefCell::new(Some(StoredLuaValue::new(lua, value)?)),
            is_dirty: Cell::new(true),
        });

        resources.insert(*guid, info);

        Ok(())
    }

    pub fn create_resource(
        &self,
        value: &LuaValue,
        type_index: TypeIndex,
        lua: &mlua::Lua,
    ) -> mlua::Result<ResourceId> {
        if !self.has_feature(AppFeatures::PATCH) {
            return Err(LuaError::generic("game.assets.write is unavailable"));
        }
        let resources = self.resources.borrow_mut();
        let mut guid: ResourceId;

        loop {
            let uuid = uuid::Uuid::new_v4().into_bytes();

            guid =
                kfc_format::resource_id_for_type(&self.type_registry, type_index, uuid.into(), 0)
                    .ok_or_else(|| {
                    LuaError::generic("resource type not found in current game registry")
                })?;

            if !resources.contains_key(&guid) {
                break;
            }
        }

        drop(resources);

        self.add_resource(value, &guid, lua)?;

        Ok(guid)
    }

    pub fn get_content(&self, guid: &ContentHash) -> mlua::Result<Option<Vec<u8>>> {
        if let Some(value) = self.new_contents.borrow().get(guid) {
            // OPTIMIZE: cloning here can be quite expensive
            Ok(Some(value.clone()))
        } else {
            // OPTIMIZE: consider using a reader for this
            Ok(self.reader()?.read_content(guid)?)
        }
    }

    pub fn create_content(&self, data: &[u8]) -> mlua::Result<ContentHash> {
        if !self.has_feature(AppFeatures::PATCH) {
            return Err(LuaError::generic("game.assets.write is unavailable"));
        }
        let guid = ContentHash::from_data(data);

        if let hash_map::Entry::Vacant(e) = self.new_contents.borrow_mut().entry(guid) {
            self.writer
                .borrow_mut()
                .as_mut()
                .expect("KFCWriter no longer available")
                .write_content(&guid, data)?;

            // TODO: attach a reader to the writer to avoid cloning
            e.insert(data.to_vec());
        }

        Ok(guid)
    }
}

#[cfg(test)]
mod asset_transaction_tests {
    use super::*;
    use std::{fs, path::PathBuf as StdPathBuf};

    fn fixture() -> (StdPathBuf, AppState) {
        let root = std::env::temp_dir().join(format!("sf-session-assets-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("mods")).unwrap();
        for extension in ["kfc", "kfc_resources"] {
            fs::write(root.join(format!("game.{extension}")), []).unwrap();
        }
        let type_registry = Rc::new(TypeRegistry::default());
        KFCWriter::<Rc<KFCFile>, _>::new(&root, "game", type_registry.clone(), "fixture")
            .unwrap()
            .finalize()
            .unwrap();
        let root_utf8 = PathBuf::from_path_buf(root.clone()).unwrap();
        let env = ModEnvironment::load(&root_utf8).unwrap();
        let ref_file = crate::game_files::load_kfc_file(root_utf8.join("game.kfc")).unwrap();
        let reader =
            crate::game_files::create_reader_with_extension(&root_utf8, "game", "kfc").unwrap();
        let stage = shroudforge_game_assets::transaction::begin(&root, "game").unwrap();
        for extension in ["kfc", "kfc_resources"] {
            let name = format!("game.{extension}");
            fs::copy(root.join(&name), stage.join(&name)).unwrap();
        }
        let writer = crate::game_files::create_writer(
            &PathBuf::from_path_buf(stage).unwrap(),
            "game",
            &type_registry,
            &ref_file,
        )
        .unwrap();
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
                feature_flags: AppFeatures::PATCH,
                export_dir: root_utf8.join("exports"),
                export_touches: None,
            },
            type_registry,
            attribute_catalog: OnceCell::new(),
            inline_fields: RefCell::default(),
            inline_field_bytes: Cell::new(0),
            ref_file,
            reader: RefCell::new(Some(reader)),
            writer: RefCell::new(Some(writer)),
            asset_transaction: Cell::new(true),
            asset_stem: "game".to_owned(),
            new_contents: RefCell::default(),
            content_reads_in_flight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            resources: RefCell::default(),
            loaded_resource_indexes: RefCell::default(),
            types: RefCell::default(),
        };
        (root, state)
    }

    #[test]
    fn publication_releases_kfc_cursor_before_replacing_live_files() {
        let (root, state) = fixture();
        let content = state.create_content(b"new-content").unwrap();
        state.take_writer().unwrap().finalize().unwrap();
        state.commit_assets().unwrap();
        let mut published = KFCReader::new(&root, "game")
            .unwrap()
            .into_cursor()
            .unwrap();
        assert_eq!(
            published.read_content(&content).unwrap().unwrap(),
            b"new-content"
        );
        assert!(!shroudforge_game_assets::transaction::directory(&root, "game").exists());
        drop(published);
        drop(state);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn abort_releases_staged_writer_before_removing_stage() {
        let (root, state) = fixture();
        let original = fs::read(root.join("game.kfc")).unwrap();
        state.create_content(b"unpublished-content").unwrap();
        drop(state);
        assert_eq!(fs::read(root.join("game.kfc")).unwrap(), original);
        assert!(!shroudforge_game_assets::transaction::directory(&root, "game").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn abort_releases_cursor_before_recovering_interrupted_publication() {
        let (root, state) = fixture();
        let original = fs::read(root.join("game.kfc")).unwrap();
        let stage = shroudforge_game_assets::transaction::directory(&root, "game");
        let mut records = Vec::new();
        for extension in ["kfc_resources", "kfc"] {
            let name = format!("game.{extension}");
            fs::copy(root.join(&name), stage.join(format!("{name}.rollback"))).unwrap();
            records.push((name, true));
        }
        fs::write(
            stage.join("pending.json"),
            serde_json::to_vec(&records).unwrap(),
        )
        .unwrap();
        fs::write(root.join("game.kfc"), b"interrupted-index").unwrap();
        drop(state);
        assert_eq!(fs::read(root.join("game.kfc")).unwrap(), original);
        assert!(!shroudforge_game_assets::transaction::directory(&root, "game").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
