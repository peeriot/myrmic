{
  local c = function(config) {
    __peeriot+: config,
  },

  env(env):: c({
    env+: env,
  }),

  //  link(left, right):: {
  //    mode: '__peeriot_link',
  //  },

  db: {
    load_from(path, prefix=null, scope='d/d/p', max_depth=null):: {
      load_from+: [{
        path: path,
        [if prefix != null then 'prefix']: prefix,
        [if scope != null then 'scope']: scope,
        [if max_depth != null then 'max_depth']: max_depth,
      }],
    },
  },

  telemetry: {
    local this = self,

    default()::
      this.logs.full(),

    // activate OpenTelemetry export for logs, metrics and traces
    opentelemetry_export(endpoint='http://localhost:4317')::
      this.logs.opentelemetry_export(endpoint)
      + this.metrics.opentelemetry_export(endpoint)
      + this.traces.opentelemetry_export(endpoint),

    logs: {
      format(f='FULL'):: {
        telemetry+: {
          logs+: {
            format: f,
          },
        },
      },

      json():: {
        telemetry+: {
          logs+: {
            format: 'JSON',
          },
        },
      },

      full():: {
        telemetry+: {
          logs+: {
            format: 'FULL',
          },
        },
      },

      compact():: {
        telemetry+: {
          logs+: {
            format: 'COMPACT',
          },
        },
      },

      pretty():: {
        telemetry+: {
          logs+: {
            format: 'PRETTY',
          },
        },
      },

      // if not set will default to read the filter from environment args
      env_filter(filter=null):: {
        telemetry+: {
          logs+: {
            [if filter != null then 'env_filter']: filter,
          },
        },
      },

      opentelemetry_export(endpoint='http://localhost:4317'):: {
        telemetry+: {
          logs+: {
            otel_endpoint: endpoint,
          },
        },
      },

      // Overrides the batch log processor's defaults (2048 queue / 512 batch / 1s delay).
      // Any argument left null keeps the OTel SDK default for that setting.
      batch(max_queue_size=null, scheduled_delay_ms=null, max_export_batch_size=null):: {
        telemetry+: {
          logs+: {
            batch+: {
              [if max_queue_size != null then 'max_queue_size']: max_queue_size,
              [if scheduled_delay_ms != null then 'scheduled_delay_ms']: scheduled_delay_ms,
              [if max_export_batch_size != null then 'max_export_batch_size']: max_export_batch_size,
            },
          },
        },
      },
    },

    metrics: {
      opentelemetry_export(endpoint='http://localhost:4317'):: {
        telemetry+: {
          metrics+: {
            otel_endpoint: endpoint,
          },
        },
      },
    },

    traces: {
      opentelemetry_export(endpoint='http://localhost:4317'):: {
        telemetry+: {
          traces+: {
            otel_endpoint: endpoint,
          },
        },
      },

      // Overrides the batch span processor's defaults (2048 queue / 512 batch / 5s delay).
      // Any argument left null keeps the OTel SDK default for that setting.
      batch(max_queue_size=null, scheduled_delay_ms=null, max_export_batch_size=null):: {
        telemetry+: {
          traces+: {
            batch+: {
              [if max_queue_size != null then 'max_queue_size']: max_queue_size,
              [if scheduled_delay_ms != null then 'scheduled_delay_ms']: scheduled_delay_ms,
              [if max_export_batch_size != null then 'max_export_batch_size']: max_export_batch_size,
            },
          },
        },
      },
    },
  },
}
