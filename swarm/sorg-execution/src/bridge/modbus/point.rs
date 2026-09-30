//! A point: where one value of a spec entry lives on the server, and its type.
//!
//! Every kind of entry (`poll`, `read`, `write`) boils down to a point. Reading or
//! writing a point combines the connection (which moves raw bits and words) with
//! the codec (which gives them a type).

use sorg_common::{
    ModbusByteOrder, ModbusRegister, ModbusValueTemplate, ModbusWritableRegister, WireModbusPoll,
    WireModbusRead, WireModbusWrite,
};

use super::codec::{self, ModbusValue};
use super::connection::{self, Connection, Error};

/// A value at `address` in the table `register` (a [`ModbusRegister`] to read, a
/// [`ModbusWritableRegister`] to write) of unit `unit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Point<R> {
    pub unit: u8,
    pub register: R,
    pub address: u16,
    pub template: ModbusValueTemplate,
    pub byte_order: ModbusByteOrder,
}

impl Point<ModbusRegister> {
    pub(crate) fn of_poll(entry: &WireModbusPoll, default_unit: u8) -> Self {
        Self {
            unit: entry.unit_id.unwrap_or(default_unit),
            register: entry.register,
            address: entry.address,
            template: entry.value.clone(),
            byte_order: entry.byte_order,
        }
    }

    pub(crate) fn of_read(entry: &WireModbusRead, default_unit: u8) -> Self {
        Self {
            unit: entry.unit_id.unwrap_or(default_unit),
            register: entry.register,
            address: entry.address,
            template: entry.value.clone(),
            byte_order: entry.byte_order,
        }
    }

    pub(crate) async fn read(&self, connection: &Connection) -> Result<ModbusValue, Error> {
        let cells = connection
            .read(connection::Read {
                unit: self.unit,
                register: self.register,
                address: self.address,
                quantity: codec::quantity(&self.template),
            })
            .await?;

        ModbusValue::decode(&self.template, self.byte_order, &cells).map_err(Error::Unexpected)
    }
}

impl Point<ModbusWritableRegister> {
    pub(crate) fn of_write(entry: &WireModbusWrite, default_unit: u8) -> Self {
        Self {
            unit: entry.unit_id.unwrap_or(default_unit),
            register: entry.register,
            address: entry.address,
            template: entry.value.clone(),
            byte_order: entry.byte_order,
        }
    }

    pub(crate) async fn write(
        &self,
        connection: &Connection,
        value: ModbusValue,
    ) -> Result<(), Error> {
        connection
            .write(connection::Write {
                unit: self.unit,
                register: self.register,
                address: self.address,
                cells: value.encode(self.byte_order),
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use sorg_tests::ModbusMockHandle;

    #[test]
    fn every_entry_kind_becomes_a_point() {
        let poll = WireModbusPoll {
            id: "temperature".to_owned(),
            register: ModbusRegister::Input,
            address: 1,
            value: "${f32:celsius}".parse().unwrap(),
            byte_order: ModbusByteOrder::Cdab,
            unit_id: None,
            interval: std::time::Duration::from_secs(1),
            on_change: false,
        };
        let read = WireModbusRead {
            id: "read_setpoint".to_owned(),
            register: ModbusRegister::Holding,
            address: 2,
            value: "${i16:setpoint}".parse().unwrap(),
            byte_order: ModbusByteOrder::Abcd,
            unit_id: None,
        };
        let write = WireModbusWrite {
            id: "pump_on".to_owned(),
            register: ModbusWritableRegister::Coil,
            address: 3,
            value: "${bool:on}".parse().unwrap(),
            byte_order: ModbusByteOrder::Abcd,
            unit_id: Some(7),
        };

        assert_eq!(
            Point::of_poll(&poll, 255),
            Point {
                unit: 255,
                register: ModbusRegister::Input,
                address: 1,
                template: ModbusValueTemplate::F32("celsius".into()),
                byte_order: ModbusByteOrder::Cdab,
            }
        );
        assert_eq!(Point::of_read(&read, 255).address, 2);
        // An entry's own unit wins over the bridge default.
        assert_eq!(Point::of_write(&write, 255).unit, 7);
    }

    #[tokio::test]
    async fn reads_a_typed_value() {
        let server = ModbusMockHandle::start().await;
        // 21.5 as f32 is 0x41AC_0000; little word order stores the low word first.
        server.set_input_registers(10, &[0x0000, 0x41AC]);

        let (connection, _task) = Connection::spawn(server.address(), Duration::from_secs(5));
        let point = Point {
            unit: 1,
            register: ModbusRegister::Input,
            address: 10,
            template: "${f32:celsius}".parse().unwrap(),
            byte_order: ModbusByteOrder::Cdab,
        };

        assert_eq!(point.read(&connection).await, Ok(ModbusValue::F32(21.5)));
    }

    #[tokio::test]
    async fn writes_a_typed_value() {
        let server = ModbusMockHandle::start().await;
        let (connection, _task) = Connection::spawn(server.address(), Duration::from_secs(5));
        let point = Point {
            unit: 1,
            register: ModbusWritableRegister::Holding,
            address: 20,
            template: "${i32:offset}".parse().unwrap(),
            byte_order: ModbusByteOrder::Abcd,
        };

        assert_eq!(point.write(&connection, ModbusValue::I32(-2)).await, Ok(()));
        assert_eq!(server.holding_registers(20, 2), Some(vec![0xFFFF, 0xFFFE]));
    }
}
