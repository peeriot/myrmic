This example implements a simple chat server.

The following command starts a runtime, so we can start running cells.
```shell
myrmic runtime start
```

In another shell, we need to configure the "entry-point".
This is how we can get into the network.
```shell
myrmic gateway
myrmic gateway --port 8080 # The default port is 8080, but you can change it.
```

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
