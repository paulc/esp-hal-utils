#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(extend_one)]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::timer::timg::TimerGroup;

#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;

use defmt_rtt as _;
use esp_backtrace as _;

use esp_radio::esp_now::{EspNow, PeerInfo, BROADCAST_ADDRESS};

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Ticker, Timer};

use core::fmt::Write;
use static_cell::StaticCell;

use esp_hal_utils::format_mac::format_mac;

extern crate alloc;

static RADIO_CTRL: StaticCell<esp_radio::Controller<'static>> = StaticCell::new();
static ESP_NOW: StaticCell<EspNow<'static>> = StaticCell::new();

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

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

    // Initialise ESP_NOW
    defmt::info!("Initialise ESP_NOW");
    let esp_radio_ctrl = RADIO_CTRL.init(esp_radio::init().unwrap());
    let wifi = peripherals.WIFI;
    let (mut controller, interfaces) =
        esp_radio::wifi::new(esp_radio_ctrl, wifi, Default::default()).unwrap();
    controller.set_mode(esp_radio::wifi::WifiMode::Sta).unwrap();
    controller.start().unwrap();

    // let esp_now = ESP_NOW.init(interfaces.esp_now);
    let esp_now = ESP_NOW.init(interfaces.esp_now);

    spawner.spawn(now_task(esp_now, 11)).unwrap();

    loop {
        Timer::after_millis(5000).await;
        defmt::info!("TICK");
    }
}

#[embassy_executor::task]
async fn now_task(esp_now: &'static mut EspNow<'static>, channel: u8) {
    esp_now.set_channel(channel).unwrap();

    defmt::info!("ESP-NOW VERSION: {}", esp_now.version().unwrap());
    defmt::info!(
        "        MAC ADDRESS: {}",
        format_mac(&esp_radio::wifi::sta_mac())
    );

    let mut msg = heapless::String::<64>::new();

    let mut ticker = Ticker::every(Duration::from_millis(5000));
    loop {
        match select(ticker.next(), esp_now.receive_async()).await {
            Either::First(_) => {
                // Ticker - send broadcast
                let status = esp_now.send_async(&BROADCAST_ADDRESS, b"<<HUB>>").await;
                defmt::info!("ESP-NOW BROADCAST: {:?}", status);
            }
            Either::Second(r) => {
                // RX ESP-NOW Packet
                defmt::info!(
                    "ESP-NOW RX: [{}]->[{}] >> {} [rssi={}]",
                    format_mac(&r.info.src_address),
                    format_mac(&r.info.dst_address),
                    match core::str::from_utf8(r.data()) {
                        Ok(s) => s,
                        Err(_) => {
                            write!(&mut msg, "{:?}", r.data()).unwrap();
                            msg.as_str()
                        }
                    },
                    r.info.rx_control.rssi
                );
                // Add peer if not known
                if !esp_now.peer_exists(&r.info.src_address) {
                    defmt::info!("ESP-NOW ADD PEER: {}", format_mac(&r.info.src_address));
                    esp_now
                        .add_peer(PeerInfo {
                            interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                            peer_address: r.info.src_address,
                            lmk: None,
                            channel: None,
                            encrypt: false,
                        })
                        .unwrap();
                    defmt::info!("ESP-NOW: peers={}", esp_now.peer_count().unwrap());
                }
                // Report RSSI to peer
                msg.clear();
                write!(
                    msg,
                    "{} -> RSSI: {}",
                    format_mac(&esp_radio::wifi::sta_mac()),
                    r.info.rx_control.rssi
                )
                .unwrap();
                let status = esp_now
                    .send_async(&r.info.src_address, msg.as_bytes())
                    .await;
                defmt::info!("ESP-NOW REPLY PEER: {:?}", status);
            }
        }
    }
    /*
    loop {
        match with_timeout(Duration::from_secs(1), esp_now.receive_async()).await {
            Ok(r) => {
                defmt::info!(
                    "ESP-NOW RX: [{}]->[{}] >> {} [rssi={}]",
                    format_mac(&r.info.src_address),
                    format_mac(&r.info.dst_address),
                    match core::str::from_utf8(r.data()) {
                        Ok(s) => s,
                        Err(_) => {
                            write!(&mut msg, "{:?}", r.data()).unwrap();
                            msg.as_str()
                        }
                    },
                    r.info.rx_control.rssi
                );
                // Add peer if not known
                if !esp_now.peer_exists(&r.info.src_address) {
                    defmt::info!("ESP-NOW ADD PEER: {}", format_mac(&r.info.src_address));
                    esp_now
                        .add_peer(PeerInfo {
                            interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                            peer_address: r.info.src_address,
                            lmk: None,
                            channel: None,
                            encrypt: false,
                        })
                        .unwrap();
                    defmt::info!("ESP-NOW: peers={}", esp_now.peer_count().unwrap());
                }
                // Report RSSI to peer
                msg.clear();
                write!(
                    msg,
                    "{} -> RSSI: {}",
                    format_mac(&esp_radio::wifi::sta_mac()),
                    r.info.rx_control.rssi
                )
                .unwrap();
                let status = esp_now
                    .send_async(&r.info.src_address, msg.as_bytes())
                    .await;
                defmt::info!("ESP-NOW REPLY PEER: {:?}", status);
            }
            Err(TimeoutError) => defmt::info!("ESP-NOW RX: WAITING"),
        }
    }
    */
}
