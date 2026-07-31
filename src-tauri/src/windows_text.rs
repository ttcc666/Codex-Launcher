pub fn decode_output_text(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    decode_windows_oem(bytes).unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(windows)]
fn decode_windows_oem(bytes: &[u8]) -> Option<String> {
    use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_OEMCP};

    if bytes.is_empty() {
        return Some(String::new());
    }
    if bytes.len() > i32::MAX as usize {
        return None;
    }

    let required = unsafe {
        MultiByteToWideChar(
            CP_OEMCP,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        )
    };
    if required <= 0 {
        return None;
    }

    let mut wide = vec![0_u16; required as usize];
    let written = unsafe {
        MultiByteToWideChar(
            CP_OEMCP,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            wide.as_mut_ptr(),
            required,
        )
    };
    (written > 0).then(|| String::from_utf16_lossy(&wide[..written as usize]))
}

#[cfg(not(windows))]
fn decode_windows_oem(_bytes: &[u8]) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_utf8_is_preserved_exactly() {
        assert_eq!(decode_output_text("中文 output".as_bytes()), "中文 output");
    }

    #[cfg(windows)]
    #[test]
    fn cp936_bytes_decode_deterministically_when_cp936_is_active() {
        use windows_sys::Win32::Globalization::GetOEMCP;

        if unsafe { GetOEMCP() } == 936 {
            assert_eq!(decode_output_text(&[0xD6, 0xD0]), "中");
        }
    }
}
