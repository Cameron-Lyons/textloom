//! The clipboard backend for platforms without a native rich clipboard, whose
//! helpers always fail.

use super::NativeItem;

const UNSUPPORTED: &str = "Native rich clipboard is not supported on this platform";

pub(super) fn publish(_item: &NativeItem, _owner: usize) -> Result<(), String> {
    Err(UNSUPPORTED.into())
}

pub(super) fn read() -> Result<Option<NativeItem>, String> {
    Err(UNSUPPORTED.into())
}
