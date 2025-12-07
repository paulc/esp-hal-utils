#![allow(unused)]

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::i2c::I2c;

#[derive(Debug)]
pub enum Aht20Error {
    I2cError,
    CrcError,
}

#[derive(Debug, Clone)]
pub struct Aht20Reading {
    pub temp: f32,
    pub rh: f32,
}

pub struct Aht20<'a, M, BUS>
where
    M: RawMutex,
    BUS: I2c,
{
    i2c: I2cDevice<'a, M, BUS>,
    address: u8,
}

impl<'a, M, BUS> Aht20<'a, M, BUS>
where
    M: RawMutex,
    BUS: I2c,
{
    pub fn new(i2c: I2cDevice<'a, M, BUS>, address: u8) -> Self {
        Self { i2c, address }
    }
    pub async fn init(&mut self) -> Result<(), Aht20Error> {
        self.i2c
            .write(self.address, &[0xBA])
            .await
            .map_err(|_| Aht20Error::I2cError)?;
        // Wait 40ms (reset time)
        embassy_time::Timer::after_millis(40).await;

        // Initialize (normal mode, enable heater off, enable CRC)
        self.i2c
            .write(self.address, &[0xBE, 0x08, 0x00])
            .await
            .map_err(|_| Aht20Error::I2cError)?;
        // Wait for calibration
        embassy_time::Timer::after_millis(10).await;
        Ok(())
    }
    pub async fn read(&mut self) -> Result<Aht20Reading, Aht20Error> {
        self.i2c
            .write(self.address, &[0xAC, 0x33, 0x00])
            .await
            .map_err(|_| Aht20Error::I2cError)?;

        let mut buf = [0u8; 7];
        // Poll device for ready instead of waiting 80ms
        loop {
            self.i2c
                .read(self.address, &mut buf[..1])
                .await
                .map_err(|_| Aht20Error::I2cError)?;
            if buf[0] & 0x80 == 0 {
                // Bit 7 = 0 means not busy
                break;
            }
            embassy_time::Timer::after_millis(10).await;
        }

        // Read result
        self.i2c
            .read(self.address, &mut buf)
            .await
            .map_err(|_| Aht20Error::I2cError)?;

        // Check CRC
        if crc8(&buf[..6]) != buf[6] {
            return Err(Aht20Error::CrcError);
        }

        let rh = (((buf[1] as u32) << 12) | ((buf[2] as u32) << 4) | ((buf[3] as u32) >> 4)) as f32;
        let temp =
            ((((buf[3] as u32) & 0x0F) << 16) | ((buf[4] as u32) << 8) | (buf[5] as u32)) as f32;

        Ok(Aht20Reading {
            temp: temp * 200.0 / 1_048_576.0 - 50.0,
            rh: rh * 100.0 / 1_048_576.0,
        })
    }
}

fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0xFFu8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x31
            } else {
                crc << 1
            };
        }
    }
    crc
}
