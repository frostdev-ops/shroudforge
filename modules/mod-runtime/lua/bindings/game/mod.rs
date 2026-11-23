use crate::env::AppState;

mod assets;
mod content_id_casts;
mod guid;
pub(crate) mod resolve_type;
mod types;
// Kiln layer: what the type metadata game.types hands out holds
#[cfg(test)]
mod kiln_types_tests;

pub(crate) use crate::lua::resource_values as value;
pub use assets::{Content, Resource};
pub use types::Type;

pub fn create(lua: &mlua::Lua, r#mod: &mod_loader::Mod) -> mlua::Result<mlua::Table> {
    let table = lua.create_table()?;
    let app_state = lua.app_data_ref::<AppState>().unwrap();

    table.raw_set("version", app_state.kfc_file().game_version())?;
    table.raw_set("assets", assets::create(lua, r#mod)?)?;
    table.raw_set("types", types::create(lua)?)?;
    table.raw_set("guid", guid::create(lua)?)?;

    Ok(table)
}
