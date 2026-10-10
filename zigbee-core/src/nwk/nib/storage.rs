//! Flash persistence of the NIB (see [`crate::storage`]).

use byte::BytesExt;
use embedded_storage_async::nor_flash::NorFlash;

use super::Nib;
use super::NibId;
use crate::storage::flash::FlashMap;
use crate::storage::flash::Shadow;
use crate::storage::flash::round_down;
use crate::storage::flash::round_up;

// upper byte of the flash map key namespaces the NIB
const TAG: u16 = 0x0000;

// Keep independent last-successful images: storing a neighbor table must not
// invalidate the security counter headroom, or vice versa.
pub(crate) struct Shadows {
    security_material: Shadow,
    neighbor_table: Shadow,
}

impl Shadows {
    pub(crate) const fn new() -> Self {
        Self {
            security_material: ([0; crate::storage::flash::DATA], 0),
            neighbor_table: ([0; crate::storage::flash::DATA], 0),
        }
    }
}

/// Restores all persisted NIB fields; missing or unparsable items keep
/// their defaults.
pub(crate) async fn restore<F: NorFlash>(map: &mut FlashMap<F>, nib: &Nib) {
    for id in NibId::VARIANTS {
        let Some(key) = id.storage_key() else {
            continue;
        };
        let key = TAG | u16::from(key);
        if let Some(data) = map.fetch(key).await
            && !nib.import_field(*id, data)
        {
            log::warn!("stored NIB field {key:#06x} did not parse; using default");
        }
    }
    // restore does not count as modification
    let _ = nib.take_dirty();
}

/// Persists all NIB fields modified since the last call.
pub(crate) async fn flush<F: NorFlash>(map: &mut FlashMap<F>, shadows: &mut Shadows, nib: &Nib) {
    let dirty = nib.take_dirty();
    if dirty == 0 {
        return;
    }

    for id in NibId::VARIANTS {
        if dirty & (1 << (*id as u64)) == 0 {
            continue;
        }
        let Some(key) = id.storage_key() else {
            continue;
        };

        let len = if *id == NibId::security_material_set {
            // normalize counters so the stored image only changes when a
            // counter crosses its headroom boundary
            let mut set = ::core::clone::Clone::clone(&*nib.security_material_set());
            for material in set.iter_mut() {
                material.outgoing_frame_counter = round_up(material.outgoing_frame_counter);
                for entry in material.incoming_frame_counter_set.iter_mut() {
                    entry.incoming_frame_counter = round_down(entry.incoming_frame_counter);
                }
            }
            let mut offset = 0;
            let Ok(()) = map.data.write_with(&mut offset, set, byte::LE) else {
                continue;
            };
            offset
        } else {
            let Some(len) = nib.export_field(*id, &mut map.data) else {
                continue;
            };
            len
        };

        let shadow = match *id {
            NibId::security_material_set => Some(&mut shadows.security_material),
            NibId::neighbor_table => Some(&mut shadows.neighbor_table),
            _ => None,
        };
        if let Some(previous) = shadow.as_ref()
            && previous.1 == len
            && previous.0[..len] == map.data[..len]
        {
            continue;
        }

        let key = TAG | u16::from(key);
        if map.store(key, len).await {
            // Only successful writes become the baseline. On failure, the old
            // image remains so a later flush still attempts the changed value.
            if let Some(shadow) = shadow {
                shadow.0[..len].copy_from_slice(&map.data[..len]);
                shadow.1 = len;
            }
        } else {
            // retry at the next flush
            nib.mark_dirty(*id);
            log::debug!("storing NIB field {key:#06x} failed");
        }
    }
}
