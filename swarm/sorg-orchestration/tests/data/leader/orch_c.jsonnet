local z = import "zenoh.libsonnet";

z.peer('1619e204bc90ec6fa7870dac7842dac5')
+ z.plugins.dev({
  orchestration: {},
})
