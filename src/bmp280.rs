#![allow(unused)]

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::i2c::I2c;

const ID: u8 = 0xD0;
const RESET: u8 = 0xE0;
const STATUS: u8 = 0xF3;
const CTRL_MEAS: u8 = 0xF4;
const CONFIG: u8 = 0xF5;
const PRESSURE: u8 = 0xF7;
const TEMP: u8 = 0xFA;
const CALIB: u8 = 0x88;

#[derive(Debug)]
pub enum Bme280Error {
    I2cError,
    NoCalibrationData,
}

#[derive(Debug)]
pub enum Mode {
    Sleep = 0b00,
    Forced = 0b01,
    Normal = 0b11,
}

#[derive(Debug)]
pub enum Oversample {
    Skip = 0b000,
    X1 = 0b001,
    X2 = 0b010,
    X4 = 0b011,
    X8 = 0b100,
    X16 = 0b101,
}

#[derive(Debug)]
pub enum Filter {
    Off = 0x000,
    X2 = 0b001,
    X4 = 0b010,
    X8 = 0b011,
    X16 = 0b100,
}

#[derive(Debug)]
pub enum Standby {
    T0_5 = 0b000,
    T62_5 = 0b001,
    T125 = 0b010,
    T250 = 0b011,
    T500 = 0b100,
    T1000 = 0b101,
    T2000 = 0b110,
    T4000 = 0b111,
}

#[derive(Debug, Clone)]
pub struct Bmp280Reading {
    pub temp: f32,
    pub pressure: f32,
}

pub struct Bmp280<'a, M, BUS>
where
    M: RawMutex,
    BUS: I2c,
{
    i2c: I2cDevice<'a, M, BUS>,
    address: u8,
    t_calib: Option<[f32; 3]>,
    p_calib: Option<[f32; 9]>,
    t_fine: f32,
}

impl<'a, M, BUS> Bmp280<'a, M, BUS>
where
    M: RawMutex,
    BUS: I2c,
{
    pub fn new(i2c: I2cDevice<'a, M, BUS>, address: u8) -> Self {
        Self {
            i2c,
            address,
            t_calib: None,
            p_calib: None,
            t_fine: 0.0,
        }
    }
    pub async fn init(
        &mut self,
        mode: Mode,
        os_t: Oversample,
        os_p: Oversample,
        t_sb: Standby,
        filter: Filter,
    ) -> Result<(), Bme280Error> {
        self.calibrate().await?;
        self.set_config(t_sb, filter).await?;
        self.set_ctrl_meas(os_t, os_p, mode).await?;
        self.wait().await?;
        Ok(())
    }
    pub async fn init_default(&mut self) -> Result<(), Bme280Error> {
        self.calibrate().await?;
        // Default: normal mode, X1 oversampling, 16x pressure filter, 500ms standby
        self.set_config(Standby::T500, Filter::X16).await?;
        self.set_ctrl_meas(Oversample::X1, Oversample::X16, Mode::Normal)
            .await?;
        self.wait().await?;
        Ok(())
    }
    pub async fn init_low_power(&mut self) -> Result<(), Bme280Error> {
        self.calibrate().await?;
        // Low power: sleep mode, minimal oversampling, no filter
        self.set_config(Standby::T0_5, Filter::Off).await?;
        self.set_ctrl_meas(Oversample::Skip, Oversample::X1, Mode::Sleep)
            .await?;
        self.wait().await?;
        Ok(())
    }
    pub async fn wait(&mut self) -> Result<(), Bme280Error> {
        // Wait for completion
        loop {
            let (measuring, _) = self.status().await?;
            if !measuring {
                break Ok(());
            }
            embassy_time::Timer::after_millis(10).await;
        }
    }
    pub async fn measure(&mut self) -> Result<Bmp280Reading, Bme280Error> {
        Ok(Bmp280Reading {
            temp: self.temp().await?,
            pressure: self.pressure().await?,
        })
    }
    pub async fn force_measurement(&mut self) -> Result<Bmp280Reading, Bme280Error> {
        self.set_ctrl_meas(Oversample::X2, Oversample::X16, Mode::Forced)
            .await?;
        let temp = self.temp().await?;
        let pressure = self.pressure().await?;

        // Return to sleep to save power
        self.set_ctrl_meas(Oversample::X2, Oversample::X16, Mode::Sleep)
            .await?;

        Ok(Bmp280Reading { temp, pressure })
    }
    pub async fn id(&mut self) -> Result<u8, Bme280Error> {
        let mut buf = [0_u8; 1];
        self.read_register(ID, &mut buf).await?;
        Ok(buf[0])
    }
    pub async fn reset(&mut self) -> Result<(), Bme280Error> {
        let buf: [u8; 2] = [RESET, 0xB6];
        self.i2c
            .write(self.address, &buf)
            .await
            .map_err(|_| Bme280Error::I2cError)
    }
    pub async fn status(&mut self) -> Result<(bool, bool), Bme280Error> {
        let mut buf = [0_u8; 1];
        self.read_register(STATUS, &mut buf).await?;
        // (measuring,im_update)
        Ok((buf[0] & 0b0000_1000 != 0, buf[0] & 0b0000_0001 != 0))
    }
    pub async fn set_ctrl_meas(
        &mut self,
        os_t: Oversample,
        os_p: Oversample,
        mode: Mode,
    ) -> Result<(), Bme280Error> {
        let buf: [u8; 2] = [
            CTRL_MEAS,
            ((os_t as u8) << 5) | ((os_p as u8) << 2) | mode as u8,
        ];
        self.i2c
            .write(self.address, &buf)
            .await
            .map_err(|_| Bme280Error::I2cError)
    }
    pub async fn ctrl_meas(&mut self) -> Result<u8, Bme280Error> {
        let mut buf = [0_u8; 1];
        self.read_register(CTRL_MEAS, &mut buf).await?;
        Ok(buf[0])
    }
    pub async fn set_config(&mut self, t_sb: Standby, filter: Filter) -> Result<(), Bme280Error> {
        let buf: [u8; 2] = [CONFIG, ((t_sb as u8) << 5) | ((filter as u8) << 2)];
        self.i2c
            .write(self.address, &buf)
            .await
            .map_err(|_| Bme280Error::I2cError)
    }
    pub async fn config(&mut self) -> Result<u8, Bme280Error> {
        let mut buf = [0_u8; 1];
        self.read_register(CONFIG, &mut buf).await?;
        Ok(buf[0])
    }
    pub async fn temp(&mut self) -> Result<f32, Bme280Error> {
        if self.t_calib.is_none() {
            return Err(Bme280Error::NoCalibrationData);
        }
        // Wait for measurement to compete
        self.wait().await?;
        let mut buf = [0_u8; 3];
        self.read_register(TEMP, &mut buf).await?;
        let t_raw =
            (((buf[0] as u32) << 12) + ((buf[1] as u32) << 4) + ((buf[2] as u32) & 0xf)) as f32;
        self.compensate_t(t_raw)
    }
    pub async fn pressure(&mut self) -> Result<f32, Bme280Error> {
        if self.p_calib.is_none() {
            return Err(Bme280Error::NoCalibrationData);
        }
        // Wait for measurement to compete
        self.wait().await?;
        let mut buf = [0_u8; 3];
        self.read_register(PRESSURE, &mut buf).await?;
        let p_raw =
            (((buf[0] as u32) << 12) + ((buf[1] as u32) << 4) + ((buf[2] as u32) & 0xf)) as f32;
        self.compensate_p(p_raw)
    }
    pub async fn calibrate(&mut self) -> Result<(), Bme280Error> {
        let mut buf = [0u8; 24];
        self.read_register(CALIB, &mut buf).await?;
        self.t_calib = Some([
            u16::from_le_bytes([buf[0], buf[1]]) as f32,
            i16::from_le_bytes([buf[2], buf[3]]) as f32,
            i16::from_le_bytes([buf[4], buf[5]]) as f32,
        ]);
        self.p_calib = Some([
            u16::from_le_bytes([buf[6], buf[7]]) as f32,
            i16::from_le_bytes([buf[8], buf[9]]) as f32,
            i16::from_le_bytes([buf[10], buf[11]]) as f32,
            i16::from_le_bytes([buf[12], buf[13]]) as f32,
            i16::from_le_bytes([buf[14], buf[15]]) as f32,
            i16::from_le_bytes([buf[16], buf[17]]) as f32,
            i16::from_le_bytes([buf[18], buf[19]]) as f32,
            i16::from_le_bytes([buf[20], buf[21]]) as f32,
            i16::from_le_bytes([buf[22], buf[23]]) as f32,
        ]);
        Ok(())
    }
    pub async fn read_register(&mut self, register: u8, buf: &mut [u8]) -> Result<(), Bme280Error> {
        self.i2c
            .write(self.address, &[register])
            .await
            .map_err(|_| Bme280Error::I2cError)?;
        self.i2c
            .read(self.address, buf)
            .await
            .map_err(|_| Bme280Error::I2cError)
    }
    fn compensate_t(&mut self, adc_t: f32) -> Result<f32, Bme280Error> {
        match self.t_calib {
            Some([t1, t2, t3]) => {
                let var1 = (adc_t / 16384.0 - t1 / 1024.0) * t2;
                let var2 =
                    ((adc_t / 131072.0 - t1 / 8192.0) * (adc_t / 131072.0 - t1 / 8192.0)) * t3;
                self.t_fine = var1 + var2;
                Ok((var1 + var2) / 5120.0)
            }
            None => Err(Bme280Error::NoCalibrationData),
        }
    }
    fn compensate_p(&self, adc_p: f32) -> Result<f32, Bme280Error> {
        match self.p_calib {
            Some([p1, p2, p3, p4, p5, p6, p7, p8, p9]) => {
                let mut var1: f32;
                let mut var2: f32;
                let mut p: f32;
                var1 = (self.t_fine / 2.0) - 64000.0;
                var2 = (var1 * var1 * p6) / 32768.0;
                var2 = var2 + var1 * p5 * 2.0;
                var2 = (var2 / 4.0) + (p4 * 65536.0);
                var1 = (p3 * var1 * var1 / 524288.0 + p2 * var1) / 524288.0;
                var1 = (1.0 + var1 / 32768.0) * p1;
                if var1 == 0.0 {
                    Ok(0.0)
                } else {
                    p = 1048576.0 - adc_p;
                    p = (p - (var2 / 4096.0)) * 6250.0 / var1;
                    var1 = p9 * p * p / 2147483648.0;
                    var2 = p * p8 / 32768.0;
                    p = p + (var1 + var2 + p7) / 16.0;
                    // Convert from Pa -> HPa
                    Ok(p / 100.0)
                }
            }
            None => Err(Bme280Error::NoCalibrationData),
        }
    }
}
