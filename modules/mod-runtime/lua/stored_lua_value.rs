// SPDX-License-Identifier: MIT
//! Kiln layer: a Lua value a Rust object keeps between calls, held in Lua's registry instead of as an `mlua::Value`.
//!
//! An `mlua::Value` that holds a Lua object (a string, table, function or userdata) pins one slot of mlua's auxiliary
//! stack for as long as it lives, and that stack has about a million slots: past them mlua panics with "cannot create a
//! Lua reference, out of auxiliary stack space". The values of an asset tree kept one such value for every child a mod
//! visited (and a value a mod made, one for each of its fields and elements), so a mod that walked a large asset (every
//! recipe of the recipe registry, say) ran the whole startup pass into that panic. A registry entry has no such limit.
//! [`StoredLuaValue::load`] gives the value as a short-lived `mlua::Value` for one operation: the same Lua object every
//! time, so identity holds and edits made through it stay.
//!
//! nil, booleans, numbers and light userdata are no Lua objects; they are kept as they are. The registry entry is freed
//! when the last clone drops (while the Lua state still lives), so a released tree is collected as before.

use std::{fmt, rc::Rc};

use crate::lua::LuaValue;

#[derive(Clone)]
pub(crate) struct StoredLuaValue(Stored);

#[derive(Clone)]
enum Stored {
    /// A value without a Lua object: it pins no slot.
    Plain(LuaValue),
    /// A Lua object, in the registry until the last clone drops.
    Registry(Rc<RegistrySlot>),
}

struct RegistrySlot {
    /// Weak, so a value kept by a userdata does not keep its own Lua state alive.
    lua: mlua::WeakLua,
    key: Option<mlua::RegistryKey>,
}

impl StoredLuaValue {
    pub(crate) fn new(lua: &mlua::Lua, value: LuaValue) -> mlua::Result<Self> {
        Ok(Self(match value {
            LuaValue::Nil
            | LuaValue::Boolean(_)
            | LuaValue::LightUserData(_)
            | LuaValue::Integer(_)
            | LuaValue::Number(_)
            | LuaValue::Error(_) => Stored::Plain(value),
            _ => Stored::Registry(Rc::new(RegistrySlot {
                lua: lua.weak(),
                key: Some(lua.create_registry_value(value)?),
            })),
        }))
    }

    /// The value, for one operation. Keep the result no longer than that operation needs it.
    pub(crate) fn load(&self) -> mlua::Result<LuaValue> {
        match &self.0 {
            Stored::Plain(value) => Ok(value.clone()),
            Stored::Registry(slot) => {
                let closed =
                    || mlua::Error::runtime("the Lua state that held this value is closed");
                let lua = slot.lua.try_upgrade().ok_or_else(closed)?;
                lua.registry_value(slot.key.as_ref().ok_or_else(closed)?)
            }
        }
    }
}

impl Drop for RegistrySlot {
    fn drop(&mut self) {
        // While the state lives the entry is freed now, so what it held can be collected at once (mlua would only reuse
        // the slot later). When the state is closing, the key is simply dropped.
        if let (Some(key), Some(lua)) = (self.key.take(), self.lua.try_upgrade()) {
            let _ = lua.remove_registry_value(key);
        }
    }
}

impl fmt::Debug for StoredLuaValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Stored::Plain(value) => f.debug_tuple("StoredLuaValue").field(value).finish(),
            Stored::Registry(slot) => f.debug_tuple("StoredLuaValue").field(&slot.key).finish(),
        }
    }
}
