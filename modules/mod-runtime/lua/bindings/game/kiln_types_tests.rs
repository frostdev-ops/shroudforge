// SPDX-License-Identifier: MIT
//! Kiln layer: the type metadata `game.types` hands out (each type, its `struct_fields`, `enum_fields` and `attributes`)
//! is kept for the whole session, so it must not pin one slot of mlua's auxiliary stack (about a million) per value:
//! a mod reading all of it for the game's 12,658 types (39,163 struct fields, 6,119 enum fields, 10,542 type attributes)
//! held about 695,000.

use std::rc::Rc;

use kfc::reflection::TypeRegistry;

use crate::env::AppState;

/// Occupied slots of mlua's auxiliary stack: this state runs no coroutine, so the one thread in Lua's registry besides
/// the running one is mlua's reference thread, and a slot a dropped value freed is nil again.
fn auxiliary_references(lua: &mlua::Lua) -> usize {
    let mut references = 0;
    unsafe {
        lua.exec_raw::<()>((), |state| {
            use mlua::ffi;
            ffi::lua_pushnil(state);
            while ffi::lua_next(state, ffi::LUA_REGISTRYINDEX) != 0 {
                let thread = ffi::lua_tothread(state, -1);
                if !thread.is_null() && thread != state {
                    for index in 1..=ffi::lua_gettop(thread) {
                        if ffi::lua_type(thread, index) != ffi::LUA_TNIL {
                            references += 1;
                        }
                    }
                }
                ffi::lua_pop(state, 1);
            }
        })
        .unwrap();
    }
    references
}

/// U32, the enum Tag (A..D), and `structs` structs S0, S1, ...: each with fields f0..f3 (U32, Tag, U32, Tag; at 0, 4,
/// 8, 12), each field with the attributes `description` ("S<i>.f<j>", typed U32) and `compact` (no type, no value), and
/// the struct with the attribute `icon` ("type_<i>", namespace keen::ui).
fn registry(structs: usize) -> Rc<TypeRegistry> {
    let mut types = Vec::new();
    let mut push = |name: String, kind: &str, size: u32, extra: serde_json::Value| {
        let index = types.len();
        let qualified = format!("kiln_test::{name}");
        let mut entry = serde_json::json!({"index":index,"name":name,"qualifiedName":qualified,
            "impactName":name,"size":size,"alignment":4,"elementAlignment":4,"fieldCount":0,
            "primitiveType":kind,"innerType":null,"flags":"","nameHash":kfc::hash::fnv(&name),
            "qualifiedHash":kfc::hash::fnv(&qualified),"impactHash":kfc::hash::fnv(&name),"internalHash":index});
        entry
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        types.push(entry);
    };
    push("U32".into(), "UINT32", 4, serde_json::json!({}));
    push(
        "Tag".into(),
        "ENUM",
        4,
        serde_json::json!({"innerType":0,"fieldCount":4,"enumFields":{
        "A":{"name":"A","value":0},"B":{"name":"B","value":1},"C":{"name":"C","value":2},"D":{"name":"D","value":3}}}),
    );
    for i in 0..structs {
        let mut fields = serde_json::Map::new();
        for j in 0..4 {
            let name = format!("f{j}");
            fields.insert(name.clone(), serde_json::json!({"name":name,"type":j % 2,"dataOffset":j * 4,
                "attributes":{
                    "description":{"name":"description","namespace":["keen"],"type":0,"value":format!("S{i}.f{j}")},
                    "compact":{"name":"compact","namespace":["keen"]}}}));
        }
        push(
            format!("S{i}"),
            "STRUCT",
            16,
            serde_json::json!({"fieldCount":4,"structFields":fields,
            "attributes":{"icon":{"name":"icon","namespace":["keen","ui"],"type":0,"value":format!("type_{i}")}}}),
        );
    }
    Rc::new(
        serde_json::from_value(serde_json::json!({"version":"types-fixture","types":types}))
            .unwrap(),
    )
}

/// A state with `game.types` over [`registry`] as the global `types`.
fn lua_with_types(structs: usize) -> (mlua::Lua, std::path::PathBuf) {
    let (root, state) = AppState::for_tests(registry(structs));
    let lua = mlua::Lua::new();
    lua.set_app_data(state);
    lua.globals()
        .set("types", super::types::create(&lua).unwrap())
        .unwrap();
    (lua, root)
}

/// Reads every type's metadata, all the way down.
const READ_ALL: &str = r#"
    local read = 0
    for _, t in ipairs(types.get_all()) do
        for name, field in pairs(t.struct_fields) do
            assert(field.name == name and rawequal(field.type, types.get_by_index(field.type.index)))
            for key, attribute in pairs(field.attributes) do
                assert(attribute.name == key)
                for _, part in pairs(attribute.namespace) do assert(part == "keen") end
                read = read + #attribute.value
            end
        end
        for name, field in pairs(t.enum_fields) do assert(field.name == name and field.value_hex) end
        for key, attribute in pairs(t.attributes) do
            assert(attribute.name == key and attribute.namespace[2] == "ui" and rawequal(attribute.type, types.get("kiln_test::U32")))
        end
    end
    assert(read > 0)
"#;

#[test]
fn reading_all_type_metadata_leaves_mluas_reference_stack_free() {
    let mut held = Vec::new();
    for structs in [200, 2_000] {
        let (lua, root) = lua_with_types(structs);
        lua.load(READ_ALL).exec().unwrap();
        // read again: the same cached values
        lua.load(READ_ALL).exec().unwrap();
        lua.gc_collect().unwrap();
        held.push(auxiliary_references(&lua));
        drop(lua);
        std::fs::remove_dir_all(root).unwrap();
    }
    assert!(
        held[0] < 64 && held[1] <= held[0],
        "references held after reading the metadata of 200 and 2000 struct types: {held:?}"
    );
}

#[test]
fn type_metadata_keeps_its_values_order_and_identity() {
    let (lua, root) = lua_with_types(10);
    lua.load(
        r#"
        local s = types.get("kiln_test::S7")
        assert(rawequal(s, types.get_by_index(s.index)) and rawequal(s, types.get_by_qualified_name("kiln_test::S7")))
        local fields = s.struct_fields
        assert(rawequal(fields, s.struct_fields) and rawequal(s.attributes, s.attributes))
        assert(#fields == 4)
        local order = {}
        for name, field in pairs(fields) do order[#order + 1] = name .. "@" .. field.data_offset end
        assert(table.concat(order, ",") == "f0@0,f1@4,f2@8,f3@12", table.concat(order, ","))
        local f1 = fields.f1
        assert(rawequal(f1, fields.f1) and rawequal(f1.type, types.get("kiln_test::Tag")))
        assert(f1.name == "f1" and f1.data_offset == 4)
        local description, compact = f1.attributes.description, f1.attributes.compact
        assert(description.value == "S7.f1" and rawequal(description.type, types.get("kiln_test::U32")))
        assert(#description.namespace == 1 and description.namespace[1] == "keen" and description.namespace[2] == nil)
        assert(description.namespace[0] == nil)
        assert(compact.type == nil and compact.value == "" and compact.namespace[1] == "keen")
        assert(fields.missing == nil)
        local icon = s.attributes.icon
        assert(icon.value == "type_7" and icon.namespace[1] == "keen" and icon.namespace[2] == "ui")
        local list = {}
        for i, part in pairs(icon.namespace) do list[#list + 1] = i .. "=" .. part end
        assert(table.concat(list, ",") == "1=keen,2=ui")
        local tag = types.get("kiln_test::Tag")
        assert(rawequal(tag.enum_fields, tag.enum_fields) and #tag.enum_fields == 4)
        assert(tag.enum_fields.C.name == "C" and tag.enum_fields.C.value == 2)
        assert(tag.enum_fields.C.value_hex == "0x0000000000000002")
        local names = {}
        for name in pairs(tag.enum_fields) do names[#names + 1] = name end
        assert(table.concat(names) == "ABCD")
        -- the uncached view stays a fresh table each time
        assert(not rawequal(s.fields, s.fields) and s.fields[2].name == "f1")
        assert(rawequal(s.fields[2].type, f1.type))
        "#,
    )
    .exec()
    .unwrap();
    drop(lua);
    std::fs::remove_dir_all(root).unwrap();
}
