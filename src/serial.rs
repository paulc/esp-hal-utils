#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::usb_serial_jtag::{UsbSerialJtagRx, UsbSerialJtagTx};
use esp_hal::Async;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use embassy_time::Timer;

use crate::crc::crc16;
use crate::tinybuf::Buffer;

//
// Frame Layout
//
// Frame Header ----------------------> |
// Start               | Data Len (u16) | Data        | CRC (u16)
//                     | (excludes CRC) | (len bytes) |
// 0xAA 0xCC 0xAA 0xCC | LEN_L LEN_H    | DATA...     | CRC_L CRC_H
//
pub const FRAME_HEADER_LEN: usize = 6;
pub const FRAME_START: [u8; 4] = [0xaa, 0xcc, 0xaa, 0xcc];
pub const CRC_LEN: usize = 2;
pub const MAX_FRAME_LEN: usize = 2048; // Header (6 bytes), body (... bytes), crc (2 bytes)
pub const MAX_PAYLOAD_LEN: usize = MAX_FRAME_LEN - FRAME_HEADER_LEN - CRC_LEN;
pub const USB_BUFFER_LEN: usize = 64;
pub const FRAME_BUFFER_LEN: usize = MAX_FRAME_LEN + USB_BUFFER_LEN; // Ensure we have enough space from FRAME + BUFFER

pub struct FrameHeader([u8; FRAME_HEADER_LEN]);

impl FrameHeader {
    pub fn new_from_buffer<const N: usize>(buf: &mut Buffer<N>) -> Option<Self> {
        if buf.len() >= FRAME_HEADER_LEN {
            let mut header = [0_u8; FRAME_HEADER_LEN];
            buf.copy_to(&mut header[..]);
            if header.starts_with(&FRAME_START) {
                Some(Self(header))
            } else {
                None
            }
        } else {
            None
        }
    }
    pub fn get_length(&self) -> usize {
        let offset = FRAME_START.len();
        u16::from_le_bytes([self.0[offset], self.0[offset + 1]]) as usize
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FrameState {
    Wait,
    Frame(usize),
}

#[embassy_executor::task]
pub async fn frame_reader(
    mut rx: UsbSerialJtagRx<'static, Async>,
    channel: &'static Sender<'static, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
) {
    let mut usb_buf = [0u8; USB_BUFFER_LEN];
    let mut state = FrameState::Wait;
    let mut buf = Buffer::<FRAME_BUFFER_LEN>::new(); // FIFO buffer
    loop {
        let r = embedded_io_async::Read::read(&mut rx, &mut usb_buf).await;
        match r {
            Ok(len) => {
                // defmt::info!("Serial RX: {} bytes", len);
                buf.push(&usb_buf[..len]).expect("Buffer Overflow"); // safe as usb_buf < buf
                match state {
                    FrameState::Wait => {
                        while let Some(i) = buf.find(&FRAME_START) {
                            // Advance to FRAME_START
                            buf.advance(i);
                            // Get frame header
                            if let Some(hdr) = FrameHeader::new_from_buffer(&mut buf) {
                                // Advance to FRAME data
                                buf.advance(FRAME_HEADER_LEN);
                                let n = hdr.get_length();
                                if n > MAX_PAYLOAD_LEN {
                                    // Frame too long - ignore (wait for next frame)
                                    //
                                    // We cant advance the cursor as we dont know how long the
                                    // frame was and there may be a valid frame following in the
                                    // same buffer - there is a risk that the data contains
                                    // FRAME_START
                                } else {
                                    if process_frame(&mut buf, n, &channel).await {
                                        state = FrameState::Wait;
                                    } else {
                                        state = FrameState::Frame(n);
                                    }
                                }
                            } else {
                                // Dont have full header - break to stop while loop reprocessing
                                break;
                            }
                        }
                    }
                    FrameState::Frame(n) => {
                        if process_frame(&mut buf, n, &channel).await {
                            state = FrameState::Wait;
                        }
                    }
                }
            }
            Err(e) => {
                defmt::error!("USB read error: {:?}", e);
            }
        }
        Timer::after_millis(1).await; // Yield to scheduler
    }
}

#[embassy_executor::task]
pub async fn frame_writer(
    mut tx: UsbSerialJtagTx<'static, Async>,
    channel: &'static Receiver<'static, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
) {
    loop {
        let message = channel.receive().await;
        let len = u16::to_le_bytes(message.len() as u16);
        let crc = u16::to_le_bytes(crc16(&message));
        // Send header
        embedded_io_async::Write::write_all(&mut tx, &FRAME_START)
            .await
            .unwrap();
        embedded_io_async::Write::write_all(&mut tx, &len)
            .await
            .unwrap();
        // Send data
        embedded_io_async::Write::write_all(&mut tx, &message)
            .await
            .unwrap();
        // Send CRC
        embedded_io_async::Write::write_all(&mut tx, &crc)
            .await
            .unwrap();
        embedded_io_async::Write::flush(&mut tx).await.unwrap();
    }
}

async fn process_frame<const N: usize>(
    buf: &mut Buffer<N>,
    payload_len: usize,
    channel: &'static Sender<'static, NoopRawMutex, heapless::Vec<u8, MAX_PAYLOAD_LEN>, 1>,
) -> bool {
    if buf.len() >= payload_len + CRC_LEN {
        // We have full frame - check CRC
        let data = buf.as_slice();
        if crc16(&data[..payload_len])
            == u16::from_le_bytes([data[payload_len], data[payload_len + 1]])
        {
            // Valid frame
            // defmt::info!(
            //     ">> RX FRAME:: [{} bytes] {:?}...",
            //     payload_len,
            //     data[..payload_len.min(8)]
            // );
            // Send to channel
            let vec: heapless::Vec<u8, MAX_PAYLOAD_LEN> =
                data[..payload_len].try_into().expect("Frame too long");
            channel.send(vec).await;
            buf.advance(payload_len + CRC_LEN); // Advance past CRC
        } else {
            // Invalid frame
            defmt::error!(">> CRC ERROR");
            buf.advance(payload_len + CRC_LEN); // Discard frame + CRC
        }
        true // Processed frame
    } else {
        false // Waiting for data
    }
}
