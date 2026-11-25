// SPDX-License-Identifier: MIT
//! Kiln layer: whole startup passes on a small, valid game archive, where one package reads what an earlier package of
//! the same pass made.
//!
//! - A blob (content) one package creates is there for `game.assets.get_content` in the packages after it, as in EML
//!   (its `has_content`: the pass's new contents, then the game's header). EMF Beastmaster's beasts pack writes the
//!   English text table back as a new blob, and the wilds pack's harness script reads it: with only the game's header
//!   asked, it got nil and the pass went on without the Llama Harness, Camel Saddle and Mule Harness (0.2.0-dev.31.13).
//! - A texture the pass encodes to BC7 is cut into strips on several threads and is byte for byte upstream's serial
//!   encode (mod-runtime/lua/bindings/image/kiln_encode_in_strips.rs).

use super::kiln_retry_tests::Game;
use super::*;
use kfc::container::KFCReader;
use kfc::guid::ContentHash;

fn published(game: &Game, hash: &ContentHash) -> Option<Vec<u8>> {
    KFCReader::new(&game.0, "enshrouded")
        .unwrap()
        .into_cursor()
        .unwrap()
        .read_content(hash)
        .unwrap()
}

fn ids(list: &serde_json::Value) -> Vec<&str> {
    list.as_array()
        .map(|items| items.iter().filter_map(|item| item["id"].as_str()).collect())
        .unwrap_or_default()
}

const TEXT: &str = "keenglish: Llama Harness\tCamel Saddle\tMule Harness";

#[test]
fn a_blob_one_package_creates_is_there_for_the_next_package_of_the_pass() {
    let game = Game::new("same-pass-blob");
    let text = ContentHash::from_data(TEXT.as_bytes());
    let derived = format!("seen: {TEXT}");
    let unknown = ContentHash::from_data(b"no package made this");
    // the beasts pack: writes the text table back as a new blob
    game.package(
        "beasts",
        &format!("game.assets.create_content(buffer.wrap({TEXT:?}))\nreturn {{}}"),
        &[],
    );
    // the wilds pack: reads that blob by its hash (in the real pack, from the resource the beasts pack changed)
    game.package(
        "wilds",
        &format!(
            r#"
local content = game.assets.get_content("{text}")
if content == nil then error("the blob the beasts pack made is not there") end
if content.size ~= {size} then error("size " .. tostring(content.size)) end
local data = content:read_data():to_string()
if data ~= {TEXT:?} then error("other bytes: " .. data) end
if game.assets.get_content("{unknown}") ~= nil then error("a blob nobody made") end
game.assets.create_content(buffer.wrap("seen: " .. data))
return {{}}
"#,
            size = TEXT.len()
        ),
        &["beasts"],
    );
    run_startup(&game.0).unwrap();
    let record = game.record();
    assert_eq!(record["status"], "applied", "{record}");
    assert_eq!(ids(&record["mods"]), ["beasts", "wilds"], "{record}\n{}", game.log());
    assert_eq!(record.get("skipped"), None, "{record}");
    assert_eq!(published(&game, &text).as_deref(), Some(TEXT.as_bytes()));
    assert_eq!(
        published(&game, &ContentHash::from_data(derived.as_bytes())).as_deref(),
        Some(derived.as_bytes()),
        "the reader's own blob, made from what it read"
    );
    assert_eq!(published(&game, &unknown), None);
}

/// The pixels the Lua below sets, as RGBA8.
fn pixels(width: usize, height: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            out.extend_from_slice(&[
                ((x * 37 + y * 11) % 256) as u8,
                ((x * x + 3 * y) % 256) as u8,
                if (x / 3 + y / 5) % 2 == 0 { 40 } else { 220 },
                if x % 7 == 0 { 0 } else { (255 - (x + y) % 64) as u8 },
            ]);
        }
    }
    out
}

#[test]
fn a_texture_the_pass_encodes_on_several_threads_is_the_serial_encode() {
    let (width, height) = (128usize, 96usize); // 768 BC7 blocks in 24 block rows: several strips of at least 64 blocks
    let game = Game::new("same-pass-texture");
    game.package(
        "painter",
        &format!(
            r#"
local img = image.create({width}, {height})
for y = 0, {height} - 1 do
  for x = 0, {width} - 1 do
    local a = 255 - (x + y) % 64
    if x % 7 == 0 then a = 0 end
    local b = 220
    if (x // 3 + y // 5) % 2 == 0 then b = 40 end
    img:set_pixel(x, y, (x * 37 + y * 11) % 256, (x * x + 3 * y) % 256, b, a)
  end
end
game.assets.create_content(image.encode_texture(img, "BC7_srgb_block"))
return {{}}
"#
        ),
        &[],
    );
    let before = shroudforge_api::textures_encoded_in_strips();
    run_startup(&game.0).unwrap();
    let record = game.record();
    assert_eq!(record["status"], "applied", "{record}\n{}", game.log());
    let format = kfc::content::image::PixelFormat::BC7_srgb_block;
    let mut serial = vec![0u8; kfc::content::image::size_of_format(format, width, height)];
    kfc::content::image::encode(format, width, height, &pixels(width, height), &mut serial).unwrap();
    assert_eq!(
        published(&game, &ContentHash::from_data(&serial)).as_deref(),
        Some(serial.as_slice()),
        "the pass's texture is upstream's serial encode, byte for byte"
    );
    // a PC with more than one thread: the pregame pass cut it into strips (other tests' passes only add to the count)
    if std::thread::available_parallelism().map_or(1, |n| n.get()) > 1
        && std::env::var(shroudforge_api::ENCODE_THREADS_VARIABLE).is_err()
    {
        assert!(shroudforge_api::textures_encoded_in_strips() > before, "the pass encoded on one thread");
    }
}
