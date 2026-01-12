#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;

#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;

use esp_radio::esp_now::BROADCAST_ADDRESS;

use embassy_executor::Spawner;
use embassy_futures::select::{select3, Either3};
use embassy_time::{Duration, Ticker};

use esp_now_protocol::Msg;

use defmt_rtt as _;
use esp_backtrace as _;

use esp_hal_utils::serial::{frame_reader, frame_writer};

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
esp_bootloader_esp_idf::esp_app_desc!();

mod error;
mod handler;
mod serial_channel;

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    esp_alloc::heap_allocator!(size: 64 * 1024);

    defmt::info!("Init!");

    #[cfg(target_arch = "riscv32")]
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(
        timg0.timer0,
        #[cfg(target_arch = "riscv32")]
        sw_int.software_interrupt0,
    );

    // Start serial
    let (rx, tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();

    let (serial_tx_sender, serial_tx_receiver, serial_rx_sender, serial_rx_receiver) =
        serial_channel::init();

    spawner.must_spawn(frame_reader(rx, &serial_rx_sender));
    spawner.must_spawn(frame_writer(tx, &serial_tx_receiver));

    // Start ESP-NOW
    defmt::info!("Start ESP_NOW");

    let esp_radio_ctrl = esp_radio::init().unwrap();
    let wifi = peripherals.WIFI;
    let (mut controller, interfaces) =
        esp_radio::wifi::new(&esp_radio_ctrl, wifi, Default::default()).unwrap();
    controller.set_mode(esp_radio::wifi::WifiMode::Sta).unwrap();
    controller.start().unwrap();

    let mut esp_now = interfaces.esp_now;
    handler::handle_init(&mut esp_now, &serial_tx_sender)
        .await
        .unwrap();

    // Main Loop
    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut broadcast: Option<(u32, heapless::Vec<u8, 250>)> = None;
    let mut broadcast_counter: u32 = 0;

    loop {
        match select3(
            ticker.next(),
            serial_rx_receiver.receive(),
            esp_now.receive_async(),
        )
        .await
        {
            Either3::First(_) => {
                // Ticker - check broadcast interval
                if let Some((ref interval, ref data)) = broadcast {
                    if broadcast_counter > 0 && broadcast_counter.is_multiple_of(*interval) {
                        // Send broadcast
                        let status = esp_now.send_async(&BROADCAST_ADDRESS, &data).await;
                        defmt::info!(">> BROADCAST -> {:?}", status);
                        broadcast_counter = 0;
                    } else {
                        broadcast_counter += 1;
                    }
                }
            }
            Either3::Second(serial_frame) => {
                // Received serial frame
                // Try to decode message
                match Msg::from_slice(&serial_frame) {
                    Ok(msg) => {
                        defmt::info!("RX MSG: {}", msg);
                        match msg {
                            Msg::Send(data) => {
                                match handler::handle_send(&mut esp_now, &serial_tx_sender, &data)
                                    .await
                                {
                                    Ok(_) => defmt::info!(">>> Msg::Send OK"),
                                    Err(e) => defmt::error!(">>> Msg::Send Error: {}", e),
                                }
                            }
                            Msg::Broadcast(data) => {
                                match handler::handle_broadcast(
                                    &mut esp_now,
                                    &serial_tx_sender,
                                    &data,
                                )
                                .await
                                {
                                    Ok(_) => defmt::info!(">>> Msg::Broadcast OK"),
                                    Err(e) => defmt::error!(">>> Msg::Broadcast Error: {}", e),
                                }
                                // Set broadcast interval timer
                                if let Some(interval) = data.interval {
                                    broadcast = Some((interval, data.data));
                                    broadcast_counter = 0; // Reset counter
                                }
                            }
                            _ => {}
                        }
                    }
                    Err(_) => defmt::error!(
                        ">>> Invalid Msg: {} bytes - {:?}",
                        serial_frame.len(),
                        serial_frame[..serial_frame.len().min(8)]
                    ),
                }
            }
            Either3::Third(rx_data) => {
                /*
                defmt::info!(
                    ">> ESP-NOW RX: [{}]->[{}] >> {} [rssi={}]",
                    format_mac(&rx_data.info.src_address),
                    format_mac(&rx_data.info.dst_address),
                    match core::str::from_utf8(rx_data.data()) {
                        Ok(s) => s,
                        Err(_) => "<<DATA>>",
                    },
                    rx_data.info.rx_control.rssi
                );
                */
                match handler::handle_recv(&serial_tx_sender, &rx_data).await {
                    Ok(_) => defmt::info!(">>> Msg::Recv OK"),
                    Err(e) => defmt::error!("Msg::Recv Error: {}", e),
                }
            }
        }
    }
}
