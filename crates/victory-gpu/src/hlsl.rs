//! The layer's own shaders, as HLSL in `hlsl/`, compiled beforehand
//! (`hlsl/build.sh`, with the open DirectX shader compiler; not kept in the
//! repository).
//! Wine's `D3DCompile` lacks parts of HLSL they use, so the system's compiler
//! only gets what has no compiled form here (shaders of tests) or whose
//! source has changed since.

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::core::PCSTR;

/// Declares each shader's source and the table of compiled forms.
macro_rules! shaders {
    ($($name:ident $file:literal $entry:literal;)*) => {
        $(#[allow(dead_code)] pub(crate) const $name: &str = include_str!(concat!("hlsl/", $file, ".hlsl"));)*
        static COMPILED: &[(&str, &str, &str, &[u8])] = &[$((
            $name,
            $entry,
            include_str!(concat!(env!("OUT_DIR"), "/", $file, ".sum")),
            include_bytes!(concat!(env!("OUT_DIR"), "/", $file, ".dxil")),
        ),)*];
    };
}

shaders! {
    CLAMP "clamp" "main";
    WINDOW_CHECK "window_check" "main";
    TEST_WORKER "test_worker" "main";
    CARD_COMPARE "card_compare" "main";
    CARD_COMPARE_WIDE "card_compare_wide" "main";
    CARD_FETCH "card_fetch" "main";
    PLANE_CORNERS "plane_corners" "corner";
    PLANE_STEPS "plane_steps" "plane";
    PLANE_FLOATS "plane_floats" "plane";
}

/// A fingerprint of `source` that a checkout with other line ends does not change.
fn sum(source: &str) -> String {
    blake3::hash(source.replace('\r', "").as_bytes()).to_hex().to_string()
}

/// The compiled form of function `entry` of `source`, unless it is stale.
fn compiled(source: &str, entry: &[u8]) -> Option<&'static [u8]> {
    let (_, _, of, code) = COMPILED.iter().find(|s| s.0 == source && s.1.as_bytes() == entry)?;
    (of.trim() == sum(source)).then_some(*code)
}

/// Function `entry` of `source` for `target`, with its root signature if the
/// source declares one.
pub(crate) fn compile(source: &str, entry: PCSTR, target: PCSTR) -> windows::core::Result<Vec<u8>> {
    // SAFETY: the names are literals.
    if let Some(code) = compiled(source, unsafe { entry.as_bytes() }) {
        return Ok(code.to_vec());
    }
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers ours.
    let done = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            windows::core::s!("victory"),
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    // SAFETY: a blob's own bytes, copied while it lives.
    let bytes = |b: &ID3DBlob| unsafe {
        std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()).to_vec()
    };
    if let Err(e) = done {
        if let Some(errors) = errors {
            tracing::warn!("a shader of ours: {}", String::from_utf8_lossy(&bytes(&errors)));
        }
        return Err(e);
    }
    Ok(bytes(&code.ok_or_else(windows::core::Error::empty)?))
}

#[cfg(test)]
mod tests {
    /// Every compiled form there is, is of the source beside it; run
    /// `hlsl/build.sh` if not.
    #[test]
    fn the_compiled_shaders_are_current() {
        for (source, entry, _, code) in super::COMPILED {
            if code.is_empty() {
                continue;
            }
            assert!(super::compiled(source, entry.as_bytes()).is_some(), "stale: {source}");
        }
    }
}
