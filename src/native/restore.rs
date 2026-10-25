//! Restore only the atomic state modified by the single-output smoke test.
//!
//! Smithay 0.7's device destructor replays every property of the entire card.
//! Do not replay immutable/legacy properties, fence pointers, or other outputs.

use std::io;

use smithay::{
    backend::drm::DrmDeviceFd,
    reexports::drm::control::{
        AtomicCommitFlags, Device as _, ResourceHandle, atomic::AtomicModeReq, connector, crtc,
        plane, property,
    },
};

#[derive(Clone, Copy)]
enum ObjectKind {
    Connector,
    Crtc,
    Plane,
}

fn restorable(kind: ObjectKind, name: &str, mutable: bool) -> bool {
    mutable
        && match kind {
            ObjectKind::Connector => name == "CRTC_ID",
            ObjectKind::Crtc => matches!(name, "MODE_ID" | "ACTIVE" | "VRR_ENABLED"),
            ObjectKind::Plane => matches!(
                name,
                "CRTC_ID"
                    | "FB_ID"
                    | "SRC_X"
                    | "SRC_Y"
                    | "SRC_W"
                    | "SRC_H"
                    | "CRTC_X"
                    | "CRTC_Y"
                    | "CRTC_W"
                    | "CRTC_H"
                    | "rotation"
                    | "alpha"
                    | "FB_DAMAGE_CLIPS"
                    | "IN_FENCE_FD"
            ),
        }
}

fn restore_value(name: &str, value: u64) -> u64 {
    match name {
        // These are per-commit metadata, not reusable saved resources.
        "IN_FENCE_FD" => u64::MAX, // signed -1: no input fence
        "FB_DAMAGE_CLIPS" => 0,    // repaint the whole restored framebuffer
        _ => value,
    }
}

type Properties = Vec<(String, property::Handle, u64)>;

fn properties<H: ResourceHandle>(
    fd: &DrmDeviceFd,
    object: H,
    kind: ObjectKind,
) -> io::Result<Properties> {
    let set = fd.get_properties(object)?;
    let mut saved = Vec::new();
    for (handle, value) in set {
        let info = fd.get_property(handle)?;
        let name = info.name().to_string_lossy().into_owned();
        if restorable(kind, &name, info.mutable()) {
            saved.push((name.clone(), handle, restore_value(&name, value)));
        }
    }
    Ok(saved)
}

fn required(props: &Properties, name: &str) -> io::Result<u64> {
    props
        .iter()
        .find(|(key, _, _)| key == name)
        .map(|(_, _, value)| *value)
        .ok_or_else(|| io::Error::other(format!("missing writable restore property {name}")))
}

fn add_properties<H: ResourceHandle>(request: &mut AtomicModeReq, object: H, props: &Properties) {
    for (_, property, value) in props {
        request.add_raw_property(object.into(), *property, *value);
    }
}

pub(super) struct AtomicRestore {
    fd: DrmDeviceFd,
    request: AtomicModeReq,
    mode_blob: Option<u64>,
    target: (connector::Handle, crtc::Handle, plane::Handle),
}

impl AtomicRestore {
    pub fn connector(&self) -> connector::Handle {
        self.target.0
    }

    pub fn plane(&self) -> plane::Handle {
        self.target.2
    }

    pub fn capture(
        fd: &DrmDeviceFd,
        connector: connector::Handle,
        crtc: crtc::Handle,
        plane: plane::Handle,
    ) -> io::Result<Self> {
        let connector_props = properties(fd, connector, ObjectKind::Connector)?;
        let mut crtc_props = properties(fd, crtc, ObjectKind::Crtc)?;
        let plane_props = properties(fd, plane, ObjectKind::Plane)?;
        let crtc_id = u64::from(u32::from(crtc));
        for props in [&connector_props, &plane_props] {
            let previous = required(props, "CRTC_ID")?;
            if previous != 0 && previous != crtc_id {
                return Err(io::Error::other("restore target belongs to another CRTC"));
            }
        }
        required(&plane_props, "FB_ID")?;
        let active = required(&crtc_props, "ACTIVE")?;
        let mode_id = required(&crtc_props, "MODE_ID")?;
        let mut saved = Self {
            fd: fd.clone(),
            request: AtomicModeReq::new(),
            mode_blob: None,
            target: (connector, crtc, plane),
        };
        // Mode blobs may disappear when their original CRTC state is replaced.
        // Own a fresh copy rather than retaining a borrowed property ID.
        if mode_id != 0 {
            let mode = fd
                .get_crtc(crtc)?
                .mode()
                .ok_or_else(|| io::Error::other("saved CRTC mode is unavailable"))?;
            let blob = fd.create_property_blob(&mode)?;
            let blob_id = u64::from(blob);
            saved.mode_blob = Some(blob_id);
            for (name, _, value) in &mut crtc_props {
                if name == "MODE_ID" {
                    *value = blob_id;
                }
            }
        } else if active != 0 {
            return Err(io::Error::other("active saved CRTC has no mode"));
        }
        add_properties(&mut saved.request, connector, &connector_props);
        add_properties(&mut saved.request, crtc, &crtc_props);
        add_properties(&mut saved.request, plane, &plane_props);
        // Check the inverse commit before changing the display. This does not
        // modeset; an unsupported snapshot refuses presentation entirely.
        saved.test()?;
        println!(
            "Native restore snapshot validated: connector {connector:?}, CRTC {crtc:?}, plane {plane:?}"
        );
        Ok(saved)
    }

    fn test(&self) -> io::Result<()> {
        self.fd.atomic_commit(
            AtomicCommitFlags::TEST_ONLY | AtomicCommitFlags::ALLOW_MODESET,
            self.request.clone(),
        )
    }

    pub fn restore(&self) -> io::Result<()> {
        // A stale framebuffer or hot-unplug must fail visibly, not silently
        // disable another output or fall back to the old card-wide replay.
        self.test().map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "native restore validation failed for {:?}: {err}",
                    self.target
                ),
            )
        })?;
        self.fd
            .atomic_commit(AtomicCommitFlags::ALLOW_MODESET, self.request.clone())
            .map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!("native restore commit failed for {:?}: {err}", self.target),
                )
            })?;
        println!("Native target state restored (atomic commit completed)");
        Ok(())
    }
}

impl Drop for AtomicRestore {
    fn drop(&mut self) {
        if let Some(blob) = self.mode_blob
            && let Err(err) = self.fd.destroy_property_blob(blob)
        {
            eprintln!("Failed to release native restore mode blob: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_readonly_legacy_and_unrelated_properties() {
        assert!(restorable(ObjectKind::Connector, "CRTC_ID", true));
        assert!(!restorable(ObjectKind::Connector, "DPMS", true));
        assert!(!restorable(ObjectKind::Connector, "EDID", false));
        assert!(!restorable(ObjectKind::Crtc, "OUT_FENCE_PTR", true));
        assert!(restorable(ObjectKind::Crtc, "MODE_ID", true));
        assert!(!restorable(ObjectKind::Plane, "type", false));
        assert!(!restorable(ObjectKind::Plane, "rotation", false));
        assert!(restorable(ObjectKind::Plane, "FB_ID", true));
        assert!(!restorable(
            ObjectKind::Plane,
            "unknown-driver-property",
            true
        ));
    }

    #[test]
    fn never_replays_fds_or_damage_blob_ids() {
        assert_eq!(restore_value("IN_FENCE_FD", 12), u64::MAX);
        assert_eq!(restore_value("FB_DAMAGE_CLIPS", 42), 0);
        assert_eq!(restore_value("FB_ID", 42), 42);
    }
}
