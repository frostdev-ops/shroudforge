pub use shroudforge_loader_config::{config, paths};
pub mod documents;
pub mod originals;
mod read_game_file_status;
mod request_game_file_restore;
#[path = "game_file_transaction.rs"]
pub mod transaction;
// Kiln layer: replaces an outdated working baseline where no Updater exists.
#[path = "set_aside_working_baseline.rs"]
pub mod working_baseline;
// Kiln layer: the record of the last startup asset pass, so that an unchanged start skips it.
#[path = "record_prepared_game_files.rs"]
pub mod prepared_stamp;
pub use request_game_file_restore::*;
