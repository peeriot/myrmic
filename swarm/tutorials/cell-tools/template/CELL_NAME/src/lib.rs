#![no_std]

use serde::{Deserialize, Serialize};
use myrmic_sdk_macros::cell;

// TODO: Define your cell state struct here, then implement it with #[cell].
//
// Example:
//
//   #[derive(Default, Serialize, Deserialize)]
//   struct MyCell {
//       value: i32,
//   }
//
//   #[cell]
//   impl MyCell {
//       #[init]
//       fn init() -> Self {
//           MyCell { value: 0 }
//       }
//
//       #[command]
//       fn get_value(&self) -> i32 {
//           self.value
//       }
//
//       #[command]
//       fn set_value(&mut self, value: i32) {
//           self.value = value;
//       }
//   }
