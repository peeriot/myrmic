This example implements a simple chat server.

The following command starts a runtime, so we can start running cells.
```shell
myrmic runtime start
```

In another shell, we need to configure the "entry-point".
This is how we can get into the network.
```shell
myrmic gateway --anonymous
myrmic gateway --anonymous --port 8080 # The default port is 8080, but you can change it.
```

`--anonymous` lets the browser in without a login - fine on your own machine. Without it the gateway refuses every page with `403 Forbidden`; a real deployment guards its mounts with OIDC in a [routing file](../../doc/chapters/10_reference/02_myrmic-cli/10_gateway.md#routing-configuration) instead.

Finally, we want to actually deploy our cell:

```shell
myrmic deploy
```

You should be able to open your browser to `localhost:8080/chat`,
and you'll see a chat window.
Hit connect, and you're connected.

The cell declares the routing config in the `init` function.
It uploads the assets that will be served when a browser opens the site.
The routing config's lifetime is tied to this cell's own lifetime,
so once this cell is taken down, the routing configuration is also removed.

To connect a second user, if you're on the same machine, you'll need to open a private window via Incognito Mode.
Once you've done that, navigate to `localhost:8080/chat`, and hit connect.

You can send messages!
