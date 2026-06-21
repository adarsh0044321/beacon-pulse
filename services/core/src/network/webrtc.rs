//! WebRTC native connection host management and binary packet deserialization.
//! Hooks into the core input simulator to execute remote mouse and keyboard triggers.

use anyhow::{anyhow, Result};
use byteorder::{BigEndian, ReadBytesExt};
use std::io::Cursor;
use tracing::{error, warn};

use crate::network::InputMsg;
use beacon_pulse_shared::protocol::binary_input::*;

/// Deserializes a binary input packet received over a WebRTC DataChannel.
///
/// Custom packet serialization formats (as defined in Network Protocol Spec):
///
/// Mouse Move:
/// [0]: Type (0x01)
/// [1]: Reserved/DisplayId
/// [2..3]: X coordinate (u16)
/// [4..5]: Y coordinate (u16)
/// [6..7]: Viewport width (u16)
/// [8..9]: Viewport height (u16)
///
/// Mouse Button:
/// [0]: Type (0x02 = Press, 0x03 = Release)
/// [1]: Button index (0x01 = Left, 0x02 = Right, 0x04 = Middle)
/// [2..3]: X coordinate (u16)
/// [4..5]: Y coordinate (u16)
/// [6..7]: Viewport width (u16)
/// [8..9]: Viewport height (u16)
///
/// Keyboard Key:
/// [0]: Type (0x05 = Press, 0x06 = Release)
/// [1]: Key flag (0x00 = Standard, 0x01 = Extended)
/// [2..3]: Windows Scan Code (u16)
pub fn parse_binary_input(data: &[u8]) -> Result<InputMsg> {
    if data.is_empty() {
        return Err(anyhow!("Empty binary input packet"));
    }

    let mut cursor = Cursor::new(data);
    let packet_type = cursor.read_u8()?;

    match packet_type {
        TYPE_MOUSE_MOVE => {
            let display_id = cursor.read_u8()?;
            let raw_x = cursor.read_u16::<BigEndian>()? as f32;
            let raw_y = cursor.read_u16::<BigEndian>()? as f32;
            let viewport_w = cursor.read_u16::<BigEndian>()? as u32;
            let viewport_h = cursor.read_u16::<BigEndian>()? as u32;

            // X and Y are normalized values in the 0..65535 range
            let x = raw_x / 65535.0;
            let y = raw_y / 65535.0;

            Ok(InputMsg::MouseMove {
                x,
                y,
                viewport_w,
                viewport_h,
                display_id: Some(display_id),
            })
        }
        TYPE_MOUSE_BUTTON_PRESS | TYPE_MOUSE_BUTTON_RELEASE => {
            let button = cursor.read_u8()?;
            let raw_x = cursor.read_u16::<BigEndian>()? as f32;
            let raw_y = cursor.read_u16::<BigEndian>()? as f32;
            let viewport_w = cursor.read_u16::<BigEndian>()? as u32;
            let viewport_h = cursor.read_u16::<BigEndian>()? as u32;

            let x = raw_x / 65535.0;
            let y = raw_y / 65535.0;
            let pressed = packet_type == TYPE_MOUSE_BUTTON_PRESS;

            Ok(InputMsg::MouseButton {
                button,
                pressed,
                x,
                y,
                viewport_w,
                viewport_h,
                display_id: Some(0),
            })
        }
        TYPE_KEY_PRESS | TYPE_KEY_RELEASE => {
            let flags = cursor.read_u8()?;
            let scan_code = cursor.read_u16::<BigEndian>()? as u32;
            let pressed = packet_type == TYPE_KEY_PRESS;
            let is_extended = flags == KEY_FLAG_EXTENDED;

            Ok(InputMsg::KeyPress {
                vk_code: 0, // Injected layouts use physical scan codes directly
                scan_code,
                pressed,
                is_extended,
            })
        }
        _ => Err(anyhow!("Unknown binary input packet type: {}", packet_type)),
    }
}

/// Helper structure for managing WebRTC peers and mapping input channels
pub struct WebRtcHostSession {
    pub session_id: String,
    pub target: Option<crate::CaptureTarget>,
}

impl WebRtcHostSession {
    pub fn new(session_id: String, target: Option<crate::CaptureTarget>) -> Self {
        Self { session_id, target }
    }

    /// Process a raw binary input frame received from the player's DataChannel
    pub fn handle_data_channel_message(&self, data: &[u8]) {
        match parse_binary_input(data) {
            Ok(msg) => {
                if let Err(e) = crate::input::dispatch_input(msg, self.target.clone()) {
                    warn!("Failed to dispatch remote input: {:?}", e);
                }
            }
            Err(e) => {
                error!("Error parsing DataChannel input payload: {:?}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mouse_move() {
        // [0]: TYPE_MOUSE_MOVE (0x01)
        // [1]: display_id (0x02)
        // [2..3]: X (65535 / 2 = 32767 = 0x7FFF)
        // [4..5]: Y (65535 / 4 = 16383 = 0x3FFF)
        // [6..7]: viewport_w (1920 = 0x0780)
        // [8..9]: viewport_h (1080 = 0x0438)
        let data = vec![0x01, 0x02, 0x7F, 0xFF, 0x3F, 0xFF, 0x07, 0x80, 0x04, 0x38];
        let result = parse_binary_input(&data).unwrap();
        if let InputMsg::MouseMove {
            x,
            y,
            viewport_w,
            viewport_h,
            display_id,
        } = result
        {
            assert!((x - 0.5).abs() < 0.01);
            assert!((y - 0.25).abs() < 0.01);
            assert_eq!(viewport_w, 1920);
            assert_eq!(viewport_h, 1080);
            assert_eq!(display_id, Some(2));
        } else {
            panic!("Expected MouseMove event");
        }
    }

    #[test]
    fn test_parse_key_press() {
        // [0]: TYPE_KEY_PRESS (0x05)
        // [1]: KEY_FLAG_EXTENDED (0x01)
        // [2..3]: Scan Code (0x001C = 28)
        let data = vec![0x05, 0x01, 0x00, 0x1C];
        let result = parse_binary_input(&data).unwrap();
        if let InputMsg::KeyPress {
            vk_code: _,
            scan_code,
            pressed,
            is_extended,
        } = result
        {
            assert_eq!(scan_code, 28);
            assert!(pressed);
            assert!(is_extended);
        } else {
            panic!("Expected KeyPress event");
        }
    }
}
