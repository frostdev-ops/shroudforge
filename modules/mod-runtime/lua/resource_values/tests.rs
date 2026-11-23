//! Writes into values a mod created (owned) must be checked against the same types as writes into
//! the game's mapped data: the type of the field or array element written, not the containing value.
use std::rc::Rc;

use kfc::reflection::{TypeIndex, TypeRegistry};

use super::*;

const HASH_KEY: usize = 1;
const REGISTRY: usize = 5;

// The shape of keen::RecipeRegistryResource.recipes[].knowledgeRequirement.knowledgeOrQueryId.value
fn recipe_fixture() -> Rc<TypeRegistry> {
    let mut types = Vec::new();
    for (i, (name, kind, size, inner, flags, count)) in [
        ("U32", "UINT32", 4, None, "", 0),
        ("HashKey32", "STRUCT", 4, None, "", 1),
        ("KnowledgeQueryData", "STRUCT", 8, None, "", 2),
        ("Recipe", "STRUCT", 8, None, "", 1),
        ("Recipes", "BLOB_ARRAY", 8, Some(3), "HAS_BLOB_ARRAY", 0),
        ("RecipeRegistry", "STRUCT", 8, None, "HAS_BLOB_ARRAY", 1),
    ]
    .into_iter()
    .enumerate()
    {
        let qualified = format!("resource_test::{name}");
        types.push(
            serde_json::json!({"index":i,"name":name,"qualifiedName":qualified,
            "impactName":name,"size":size,"alignment":4,"elementAlignment":4,
            "fieldCount":count,"primitiveType":kind,"innerType":inner,"flags":flags,
            "nameHash":kfc::hash::fnv(name),"qualifiedHash":kfc::hash::fnv(&qualified),
            "impactHash":kfc::hash::fnv(name),"internalHash":i}),
        );
    }
    types[1]["structFields"] =
        serde_json::json!({"value":{"name":"value","type":0,"dataOffset":0}});
    types[2]["structFields"] = serde_json::json!({
        "knowledgeOrQueryId":{"name":"knowledgeOrQueryId","type":1,"dataOffset":0},
        "compareValue":{"name":"compareValue","type":0,"dataOffset":4}});
    types[3]["structFields"] = serde_json::json!({
        "knowledgeRequirement":{"name":"knowledgeRequirement","type":2,"dataOffset":0}});
    types[5]["structFields"] =
        serde_json::json!({"recipes":{"name":"recipes","type":4,"dataOffset":0}});
    Rc::new(
        serde_json::from_value(serde_json::json!({"version":"recipe-fixture","types":types}))
            .unwrap(),
    )
}

// One game recipe, knowledgeOrQueryId 100, compareValue 0.
fn registry_bytes() -> Rc<[u8]> {
    let mut bytes = vec![0u8; 16];
    bytes[0..4].copy_from_slice(&8u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&100u32.to_le_bytes());
    bytes.into()
}

fn handle(registry: &Rc<TypeRegistry>, index: usize) -> TypeHandle {
    TypeHandle::new(registry.clone(), TypeIndex::new(index))
}

/// A Lua state with `game` (the game's mapped registry), `push` (table.insert's path for
/// resource arrays) and `own(value)` (the registry data as a value a mod created, as
/// `resource.data = copy` makes it).
fn lua_with_registry() -> (mlua::Lua, Rc<TypeRegistry>) {
    let registry = recipe_fixture();
    let lua = mlua::Lua::new();
    let bytes = registry_bytes();
    let mapped = MappedValue::from_bytes(
        &registry,
        registry.get(TypeIndex::new(REGISTRY)).unwrap(),
        &bytes,
    )
    .unwrap();
    lua.globals()
        .set("game", convert_value_to_lua(&mapped, &lua).unwrap())
        .unwrap();
    let push_fn = lua
        .create_function(|lua, (list, value): (AnyUserData, LuaValue)| {
            assert!(push(&list, &value, lua)?, "not a resource array");
            Ok(())
        })
        .unwrap();
    lua.globals().set("push", push_fn).unwrap();
    let registry_type = handle(&registry, REGISTRY);
    let own_fn = lua
        .create_function(move |lua, value: LuaValue| {
            validate_and_clone_lua_value(&value, &registry_type, lua).map_err(mlua::Error::external)
        })
        .unwrap();
    lua.globals().set("own", own_fn).unwrap();
    lua.load(
        r#"
        function plain(v)
            if type(v) ~= "userdata" then return v end
            local copy = {}
            for k, x in pairs(v) do copy[k] = plain(x) end
            return copy
        end
        function new_recipe(id)
            return { knowledgeRequirement = { knowledgeOrQueryId = { value = id }, compareValue = 0 } }
        end
        -- What Unlock Blueprints does to every recipe (mods/sf-unlock-blueprints/src/mod.lua:48).
        function unlock_all(data, id)
            for _, recipe in ipairs(data.recipes) do
                local requirement = recipe.knowledgeRequirement
                if requirement.knowledgeOrQueryId.value ~= id then
                    requirement.knowledgeOrQueryId.value = id
                end
                requirement.compareValue = 1
            end
        end
        function check_unlocked(data, id, count)
            assert(#data.recipes == count, "recipes: " .. #data.recipes)
            for i, recipe in ipairs(data.recipes) do
                local requirement = recipe.knowledgeRequirement
                assert(requirement.knowledgeOrQueryId.value == id, "recipe " .. i)
                assert(requirement.compareValue == 1, "recipe " .. i)
            end
        end
        "#,
    )
    .exec()
    .unwrap();
    (lua, registry)
}

fn assert_converts(lua: &mlua::Lua, registry: &Rc<TypeRegistry>, global: &str, ids: &[u64]) {
    let data: LuaValue = lua.globals().get(global).unwrap();
    let value = convert_lua_to_value(&data, &handle(registry, REGISTRY)).unwrap();
    let recipes = value.as_struct().unwrap().get("recipes").unwrap();
    let found: Vec<u64> = recipes
        .as_array()
        .unwrap()
        .iter()
        .map(|recipe| {
            let requirement = recipe
                .as_struct()
                .unwrap()
                .get("knowledgeRequirement")
                .unwrap();
            let id = requirement
                .as_struct()
                .unwrap()
                .get("knowledgeOrQueryId")
                .unwrap();
            id.as_struct()
                .unwrap()
                .get("value")
                .unwrap()
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(found, ids);
}

#[test]
fn a_recipe_another_mod_added_takes_an_integer_key_like_the_game_recipes() {
    // More Gliders inserts plain-table recipes into the game's registry, Unlock Blueprints then
    // writes an integer into every recipe's knowledgeOrQueryId.value (player report, dev.24).
    let (lua, registry) = lua_with_registry();
    lua.load(
        r#"
        push(game.recipes, new_recipe(2771652312))
        unlock_all(game, 1715248921)
        check_unlocked(game, 1715248921, 2)
        "#,
    )
    .exec()
    .unwrap();
    assert_converts(&lua, &registry, "game", &[1715248921, 1715248921]);
}

#[test]
fn a_registry_another_mod_replaced_takes_the_writes_the_game_registry_takes() {
    // More Gliders' fallback: copy the data into plain tables and assign the whole value back.
    let (lua, registry) = lua_with_registry();
    lua.load(
        r#"
        local copy = plain(game)
        copy.recipes[#copy.recipes + 1] = new_recipe(2771652312)
        owned = own(copy)
        unlock_all(owned, 1715248921)
        check_unlocked(owned, 1715248921, 2)
        "#,
    )
    .exec()
    .unwrap();
    assert_converts(&lua, &registry, "owned", &[1715248921, 1715248921]);
}

#[test]
fn owned_and_mapped_values_accept_and_refuse_the_same_writes() {
    let (lua, registry) = lua_with_registry();
    lua.load(
        r#"
        owned = own(plain(game))
        for _, data in ipairs({ game, owned }) do
            local recipe = data.recipes[1]
            local requirement = recipe.knowledgeRequirement
            -- a value of the field's own type: a table, the value the field holds, an integer leaf
            requirement.knowledgeOrQueryId = { value = 7 }
            requirement.knowledgeOrQueryId = requirement.knowledgeOrQueryId
            recipe.knowledgeRequirement = recipe.knowledgeRequirement
            recipe.knowledgeRequirement.knowledgeOrQueryId.value = 8
            -- an array element: the element it holds, and a table
            data.recipes[1] = data.recipes[1]
            data.recipes[1] = new_recipe(9)
            push(data.recipes, new_recipe(10))
            assert(#data.recipes == 2)
            assert(data.recipes[1].knowledgeRequirement.knowledgeOrQueryId.value == 9)
            assert(data.recipes[2].knowledgeRequirement.knowledgeOrQueryId.value == 10)
            -- still refused: wrong types, unknown fields, the containing type where a field goes
            local field = data.recipes[1].knowledgeRequirement
            for _, bad in ipairs({
                function() field.knowledgeOrQueryId.value = "x" end,
                function() field.knowledgeOrQueryId.value = -1 end,
                function() field.knowledgeOrQueryId = 5 end,
                function() field.knowledgeOrQueryId = field end,
                function() field.unknown = 1 end,
                function() data.recipes[1] = 5 end,
                function() data.recipes[1] = data.recipes end,
                function() push(data.recipes, 5) end,
            }) do
                assert(not pcall(bad))
            end
        end
        "#,
    )
    .exec()
    .unwrap();
    assert_converts(&lua, &registry, "game", &[9, 10]);
    assert_converts(&lua, &registry, "owned", &[9, 10]);
}

#[test]
fn a_struct_field_write_error_names_the_field_type() {
    let (lua, registry) = lua_with_registry();
    let owned: LuaValue = lua.load("return own(plain(game))").eval().unwrap();
    lua.globals().set("owned", owned).unwrap();
    let err = lua
        .load("owned.recipes[1].knowledgeRequirement.knowledgeOrQueryId = 5")
        .exec()
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(&format!(
            "expected `{}`",
            handle(&registry, HASH_KEY).qualified_name
        )),
        "{err}"
    );
}

// Kiln layer: the values of an asset tree, and those a mod makes, must not pin one slot of mlua's auxiliary stack
// (about a million slots) per value they keep. Player report (Forged and Found 0.16.1 on a Steam Deck, Kiln 30.5):
// "Rust panic during startup asset preparation: cannot create a Lua reference, out of auxiliary stack space (used
// 1000194 slots)".

/// Occupied slots of mlua's auxiliary stack. These states run no coroutine, so the one thread in Lua's registry besides
/// the running one is mlua's reference thread; a slot a dropped value freed is nil again.
fn auxiliary_references(lua: &mlua::Lua) -> usize {
    registry_scan(lua).0
}

/// Userdata held directly by Lua's registry.
fn registry_userdata(lua: &mlua::Lua) -> usize {
    registry_scan(lua).1
}

fn registry_scan(lua: &mlua::Lua) -> (usize, usize) {
    let (mut references, mut userdata) = (0, 0);
    unsafe {
        lua.exec_raw::<()>((), |state| {
            use mlua::ffi;
            ffi::lua_pushnil(state);
            while ffi::lua_next(state, ffi::LUA_REGISTRYINDEX) != 0 {
                if ffi::lua_type(state, -1) == ffi::LUA_TUSERDATA {
                    userdata += 1;
                }
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
    (references, userdata)
}

/// [`lua_with_registry`] with `count` game recipes; recipe i (from 0) has knowledgeOrQueryId.value i, compareValue 0.
fn lua_with_recipes(count: usize) -> (mlua::Lua, Rc<TypeRegistry>) {
    let (lua, registry) = lua_with_registry();
    let mut bytes = vec![0u8; 8 + count * 8];
    bytes[0..4].copy_from_slice(&8u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&(count as u32).to_le_bytes());
    for i in 0..count {
        bytes[8 + i * 8..12 + i * 8].copy_from_slice(&(i as u32).to_le_bytes());
    }
    let bytes: Rc<[u8]> = bytes.into();
    let mapped = MappedValue::from_bytes(
        &registry,
        registry.get(TypeIndex::new(REGISTRY)).unwrap(),
        &bytes,
    )
    .unwrap();
    lua.globals()
        .set("game", convert_value_to_lua(&mapped, &lua).unwrap())
        .unwrap();
    (lua, registry)
}

/// Visits every recipe, by `pairs` and by index, and edits a nested field of each.
const WALK: &str = r#"
    local count = 0
    for i, recipe in pairs(game.recipes) do
        local requirement = recipe.knowledgeRequirement
        assert(requirement.knowledgeOrQueryId.value == i - 1, "recipe " .. i)
        requirement.compareValue = 7
        count = count + 1
    end
    assert(count == #game.recipes)
    for i = 1, #game.recipes do
        assert(game.recipes[i].knowledgeRequirement.compareValue == 7, "recipe " .. i)
    end
"#;

/// (knowledgeOrQueryId.value, compareValue) of every recipe of the global `global`, converted back to game data.
fn converted_recipes(
    lua: &mlua::Lua,
    registry: &Rc<TypeRegistry>,
    global: &str,
) -> Vec<(u64, u64)> {
    let data: LuaValue = lua.globals().get(global).unwrap();
    let value = convert_lua_to_value(&data, &handle(registry, REGISTRY)).unwrap();
    let recipes = value.as_struct().unwrap().get("recipes").unwrap();
    recipes
        .as_array()
        .unwrap()
        .iter()
        .map(|recipe| {
            let requirement = recipe.as_struct().unwrap()["knowledgeRequirement"]
                .as_struct()
                .unwrap();
            let id = requirement["knowledgeOrQueryId"].as_struct().unwrap()["value"]
                .as_u64()
                .unwrap();
            (id, requirement["compareValue"].as_u64().unwrap())
        })
        .collect()
}

#[test]
fn a_walk_over_350000_game_recipes_leaves_mluas_reference_stack_free() {
    // plain recipe records and no mod code: what the player's startup pass panicked on
    let (lua, registry) = lua_with_recipes(350_000);
    lua.load(WALK).exec().unwrap();
    let recipes = converted_recipes(&lua, &registry, "game");
    assert_eq!(recipes.len(), 350_000);
    assert!(
        recipes
            .iter()
            .enumerate()
            .all(|(i, recipe)| *recipe == (i as u64, 7)),
        "every edit is in the converted data"
    );
}

#[test]
fn the_values_a_walk_visited_hold_no_lua_reference_each() {
    let mut held = Vec::new();
    for count in [1_000, 50_000] {
        let (lua, _) = lua_with_recipes(count);
        let before = registry_userdata(&lua);
        lua.load(WALK).exec().unwrap();
        lua.gc_collect().unwrap();
        held.push(auxiliary_references(&lua));
        // released, the whole tree is collected: no registry entry outlives the value that kept it
        lua.globals().set("game", LuaValue::Nil).unwrap();
        for _ in 0..8 {
            lua.gc_collect().unwrap();
        }
        assert_eq!(registry_userdata(&lua), before, "{count} recipes");
    }
    assert!(
        held[0] < 64 && held[1] <= held[0],
        "references held after walking 1000 and 50000 recipes: {held:?}"
    );
}

#[test]
fn a_mod_made_copy_of_350000_recipes_leaves_mluas_reference_stack_free() {
    let (lua, registry) = lua_with_registry();
    lua.load(
        r#"
        local data = { recipes = {} }
        for i = 1, 350000 do data.recipes[i] = new_recipe(i) end
        owned = own(data)
        data = nil
        assert(#owned.recipes == 350000)
        assert(owned.recipes[350000].knowledgeRequirement.knowledgeOrQueryId.value == 350000)
        owned.recipes[350000].knowledgeRequirement.compareValue = 7
        "#,
    )
    .exec()
    .unwrap();
    lua.gc_collect().unwrap();
    let references = auxiliary_references(&lua);
    assert!(
        references < 64,
        "the copy holds {references} Lua references"
    );
    let recipes = converted_recipes(&lua, &registry, "owned");
    assert_eq!(recipes.len(), 350_000);
    assert_eq!(recipes[0], (1, 0));
    assert_eq!(recipes[349_999], (350_000, 7));
}

#[test]
fn kept_values_stay_the_same_object_keep_their_edits_and_copy_apart() {
    let (lua, registry) = lua_with_registry();
    // table.remove's path for resource arrays
    let pop_fn = lua
        .create_function(|lua, list: AnyUserData| Ok(pop(&list, lua)?.0))
        .unwrap();
    lua.globals().set("pop", pop_fn).unwrap();
    let game: LuaValue = lua.globals().get("game").unwrap();
    assert!(!is_dirty_lua_value(&game).unwrap());
    lua.load(
        r#"
        local first = game.recipes[1]
        assert(rawequal(first, game.recipes[1]))
        for _, recipe in pairs(game.recipes) do assert(rawequal(recipe, first)) end
        assert(rawequal(first.knowledgeRequirement, game.recipes[1].knowledgeRequirement))
        first.knowledgeRequirement.compareValue = 5
        first = nil
        collectgarbage()
        collectgarbage()
        -- the edit stays with no Lua reference left to the edited value
        assert(game.recipes[1].knowledgeRequirement.compareValue == 5)
        -- a copy (resource.data = data) is its own tree, and so is a copy of a copy
        copy = own(game)
        assert(copy.recipes[1].knowledgeRequirement.compareValue == 5)
        copy.recipes[1].knowledgeRequirement.compareValue = 6
        game.recipes[1].knowledgeRequirement.knowledgeOrQueryId.value = 42
        assert(game.recipes[1].knowledgeRequirement.compareValue == 5)
        assert(copy.recipes[1].knowledgeRequirement.knowledgeOrQueryId.value == 100)
        assert(rawequal(copy.recipes[1], copy.recipes[1]))
        again = own(copy)
        again.recipes[1].knowledgeRequirement.compareValue = 9
        assert(copy.recipes[1].knowledgeRequirement.compareValue == 6)
        -- a removed element comes back whole, from the game's tree and from a copy
        for _, data in ipairs({ game, again }) do
            push(data.recipes, new_recipe(11))
            local removed = pop(data.recipes)
            collectgarbage()
            assert(removed.knowledgeRequirement.knowledgeOrQueryId.value == 11)
            assert(#data.recipes == 1)
        end
        "#,
    )
    .exec()
    .unwrap();
    assert!(is_dirty_lua_value(&game).unwrap());
    assert_eq!(converted_recipes(&lua, &registry, "game"), [(42, 5)]);
    assert_eq!(converted_recipes(&lua, &registry, "copy"), [(100, 6)]);
    assert_eq!(converted_recipes(&lua, &registry, "again"), [(100, 9)]);
}
