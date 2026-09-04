local z = import "zenoh.libsonnet";


  z.peer('a50ea63924a84b2fad97100faff1d761')
  + z.plugins.dev({
    execution: {
      tags: ['tag_two'],
    }
  })

