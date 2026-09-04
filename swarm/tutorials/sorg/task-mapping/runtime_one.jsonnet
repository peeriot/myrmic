local s = import 'swarm.libsonnet';
local z = import 'zenoh.libsonnet';

z.peer()
+ z.plugins.dev({
  orchestration: {},
  execution: {
    name: 'runtime 1',
    tags: ['tag_one', 'tag_three'],
  },
  db: s.db.load_from('../../target/wasm32-unknown-unknown/debug', max_depth=1),
})
