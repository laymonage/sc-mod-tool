#[cfg(feature = "uelib-bridge")]
use std::ffi::CString;
use std::path::Path;
use std::sync::atomic::AtomicBool;
#[cfg(feature = "uelib-bridge")]
use std::sync::{Mutex, PoisonError, atomic::Ordering};

use serde::Deserialize;

use crate::Error;
use crate::sound_keys;

#[cfg(feature = "uelib-bridge")]
const BRIDGE_OUTPUT_CAPACITY: usize = 16 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BridgeLink {
    pub(super) source_package: String,
    pub(super) source_object: String,
    pub(super) section: String,
    pub(super) cue: String,
    pub(super) localization_file: String,
    pub(super) sound_event: String,
    pub(super) sound_map_key: u32,
}

/// A script pattern's sound references, in script order.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BridgePattern {
    pub(super) pattern: String,
    pub(super) source_package: String,
    pub(super) calls: Vec<BridgeCall>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BridgeCall {
    pub(super) source_object: String,
    pub(super) sound_event: String,
    #[serde(default)]
    pub(super) sound_map_key: Option<u32>,
    /// Set when the sound is passed with a `Localize` cue.
    #[serde(default)]
    pub(super) section: Option<String>,
    #[serde(default)]
    pub(super) cue: Option<String>,
    /// The function the sound is passed to, such as `__NFUN_1408__`.
    #[serde(default)]
    pub(super) function: Option<String>,
}

impl BridgeCall {
    /// Whether the call speaks a line of dialogue: a comms transmission
    /// (native 1407, with a `Localize` cue) or a conversation line (native
    /// 1408). Sounds played through an actor (native 264) are effects or
    /// background voices without lines of their own.
    pub(super) fn is_dialogue(&self) -> bool {
        self.cue.is_some()
            || matches!(
                self.function.as_deref(),
                Some("__NFUN_1407__" | "__NFUN_1408__")
            )
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BridgeResult {
    // A failed analysis returns only `error`.
    #[serde(default)]
    pub(super) links: Vec<BridgeLink>,
    #[serde(default)]
    pub(super) issues: Vec<String>,
    #[serde(default)]
    pub(super) sound_packages: Vec<sound_keys::SoundPackage>,
    #[serde(default)]
    pub(super) patterns: Vec<BridgePattern>,
    #[cfg(feature = "uelib-bridge")]
    error: Option<String>,
}

pub(super) fn scan_compiled_links(
    game_root: &Path,
    cancelled: &AtomicBool,
) -> Result<BridgeResult, Error> {
    #[cfg(feature = "uelib-bridge")]
    {
        unsafe extern "C" {
            fn scmod_scan_script_audio_links(
                game_root_utf8: *const std::ffi::c_char,
                output_utf8: *mut u8,
                output_capacity: i32,
            ) -> i32;
        }

        let game_root = game_root
            .to_str()
            .ok_or_else(|| format!("Game path is not valid UTF-8: {}", game_root.display()))?;
        let game_root = CString::new(game_root)
            .map_err(|_| "Game path contains an unsupported null character.".to_owned())?;
        // UELib's decompiler keeps its state in static fields, so concurrent
        // analyses would corrupt each other's output. Run one at a time.
        static BRIDGE_LOCK: Mutex<()> = Mutex::new(());
        let _bridge_guard = BRIDGE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        // Another analysis may have held the lock for a long time.
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let mut output = vec![0u8; BRIDGE_OUTPUT_CAPACITY];
        let status = unsafe {
            scmod_scan_script_audio_links(
                game_root.as_ptr(),
                output.as_mut_ptr(),
                i32::try_from(output.len()).expect("bridge output capacity fits i32"),
            )
        };
        if status == -2 {
            return Err(format!(
                "UELib analysis output exceeded the {}-byte bridge buffer.",
                output.len()
            )
            .into());
        }

        let output_length = output
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(output.len());
        let output = std::str::from_utf8(&output[..output_length])
            .map_err(|error| format!("UELib returned invalid UTF-8: {error}"))?;
        let result: BridgeResult = serde_json::from_str(output)
            .map_err(|error| format!("Could not parse UELib analysis output: {error}"))?;
        if status != 0 {
            return Err(result
                .error
                .unwrap_or_else(|| format!("UELib analysis failed with status {status}."))
                .into());
        }
        Ok(result)
    }

    #[cfg(not(feature = "uelib-bridge"))]
    {
        let _ = (game_root, cancelled);
        Err("This build does not include the UELib bridge needed for subtitle matching.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_sound_packages_reported_by_the_bridge() {
        let result: BridgeResult = serde_json::from_str(
            r#"{
                "links": [],
                "issues": [],
                "soundPackages": [{
                    "package": "S0_0_2Voice",
                    "sounds": [{
                        "name": "Play_00_15_01",
                        "exportIndex": 40,
                        "soundMapKey": 3932201,
                        "lipSyncPath": "\\S0_0_2Voice\\00_15_01.bin"
                    }]
                }]
            }"#,
        )
        .unwrap();

        assert_eq!(result.sound_packages.len(), 1);
    }
}
