#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::usb_serial_jtag::{UsbSerialJtagRx, UsbSerialJtagTx};
use esp_hal::Async;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Receiver;

use crate::tinybuf::Buffer;

const FRAME_START: [u8; 2] = [0xaa, 0xcc];
const USB_BUFFER_LEN: usize = 64;
const MAX_FRAME_LEN: usize = 256; // Including length byte
const FRAME_HEADER_LEN: usize = 3;
const FRAME_BUFFER_LEN: usize = MAX_FRAME_LEN + USB_BUFFER_LEN; // Ensure we have enough space from FRAME + BUFFER

pub struct FrameHeader([u8; FRAME_HEADER_LEN]);

impl FrameHeader {
    pub fn new_from_buffer<const N: usize>(buf: &mut Buffer<N>) -> Option<Self> {
        if buf.len() >= FRAME_HEADER_LEN {
            let mut header = [0_u8; FRAME_HEADER_LEN];
            buf.copy_to(&mut header[..]);
            Some(Self(header))
        } else {
            None
        }
    }
    pub fn get_length(&self) -> usize {
        self.0[FRAME_HEADER_LEN - 1] as usize
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FrameState {
    Wait,
    Frame(usize),
}

#[embassy_executor::task]
pub async fn writer(
    mut tx: UsbSerialJtagTx<'static, Async>,
    channel_rx: Receiver<'static, NoopRawMutex, heapless::Vec<u8, 255>, 2>,
) {
    embedded_io_async::Write::write_all(&mut tx, b"ESP-NOW BRIDGE\r\n")
        .await
        .unwrap();
    loop {
        let message = channel_rx.receive().await;
        let len = [message.len() as u8];
        // Send header
        embedded_io_async::Write::write_all(&mut tx, &FRAME_START)
            .await
            .unwrap();
        embedded_io_async::Write::write_all(&mut tx, &len)
            .await
            .unwrap();
        embedded_io_async::Write::write_all(&mut tx, &message)
            .await
            .unwrap();
        embedded_io_async::Write::flush(&mut tx).await.unwrap();
    }
}

#[embassy_executor::task]
pub async fn frame_reader_task(mut rx: UsbSerialJtagRx<'static, Async>) {
    let mut usb_buf = [0u8; USB_BUFFER_LEN];
    let mut state = FrameState::Wait;
    let mut buf = Buffer::<FRAME_BUFFER_LEN>::new();
    loop {
        let r = embedded_io_async::Read::read(&mut rx, &mut usb_buf).await;
        match r {
            Ok(len) => {
                defmt::info!(">> Serial RX: {:?}", usb_buf[..len]);
                // Push data to buffer and look for FRAME_START
                buf.push(&usb_buf[..len]).expect("Buffer Error"); // safe
                defmt::info!("Serial RX: {}", len);
                match state {
                    FrameState::Wait => {
                        match buf.find(&FRAME_START) {
                            Some(i) => {
                                // Advance to FRAME_START
                                buf.advance(i);
                                while let Some(hdr) = FrameHeader::new_from_buffer(&mut buf) {
                                    let n = hdr.get_length();
                                    buf.advance(FRAME_HEADER_LEN);
                                    if buf.len() >= n {
                                        // We have full frame
                                        let mut frame = [0_u8; MAX_FRAME_LEN];
                                        buf.copy_to(&mut frame);
                                        buf.advance(n);
                                        state = FrameState::Wait;
                                        defmt::info!(">> RX FRAME:: {:?}", frame[..n]);
                                    } else {
                                        state = FrameState::Frame(n);
                                        defmt::info!(">> RX HEADER: {}", hdr.get_length());
                                    }
                                }
                            }
                            None => {}
                        }
                    }
                    FrameState::Frame(n) => {
                        if buf.len() >= n {
                            let mut frame = [0_u8; MAX_FRAME_LEN];
                            buf.copy_to(&mut frame);
                            buf.advance(n);
                            state = FrameState::Wait;
                            defmt::info!(">> RX FRAME:: {:?}", frame[..n]);
                        }
                    }
                }
            }
            Err(e) => {
                defmt::error!("USB read error: {:?}", e);
            }
        }
    }
}
