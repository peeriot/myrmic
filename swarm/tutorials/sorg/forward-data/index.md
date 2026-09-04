# Minimal example - Forwarding data between two zenoh topics
In this first example, we will implement a maximally simple application which uses data published on a particular zenoh topic and forwards it to a different zenoh topic. While the functionality itself is not particularly useful, if offers a good starting point to see the structure of the sorg application manifest, the configuration of the sorg plugins, and the functionality of the sorg cli tool.

## Overview
This tutorial walks you though the steps required to define and deploy the application. First, we will look at the application manifest specifying the application we want to deploy. After that, we present the swarm configuration file which defines the swarm plugins we will use. Finally, we demonstrate how you can interact with the swarm sorg layer using the provided CLI tool to init, start, and monitor the example application.

## Application manifest
The application we will be working with in this first part of the tutorial is specified by the manifest in `forward-data/manifest.yaml`.

At the time of writing, the application manifest specifies an application by describing:

- the **tasks** (atomic deployment units) that the application is made of
- the data dependencies between the tasks
- the task mapping (how the tasks should be placed on the exec runtimes on the available nodes)
- the resilience/fault tolerance mechanisms that shall be provided for the application

Three types of tasks can be defined via the application manifest:

- **source** tasks which produce data, making it available for other tasks of the same application
- **operator** tasks which consume data and produce data (typically by performing a computational operation on the data they receive)
- **sink** tasks which consume data

The minimal application that we focus on in this example consists of two tasks: a source task which "produces" data by subscribing to the zenoh topic `forward_data/source` and a sink task which publishes the data from its input onto the zenoh topic `forward_data/sink`. Since the input of the sink task is connected to the output of the source task, the application will forward any data published on the topic `forward_data/source` to the topic `forward_data/sink`.

## Example infrastructure
To test the application behavior, we will be working with two executables: `publisher` and `subscriber`. `publisher` periodically publishes data on `forward_data/source`, while `subscriber` subscribes to `forward_data/sink` and prints out information about the data it receives.

### Building the example infrastructure
Each example in this repo provides a build script. The commands below are expected to be run from `[repo-root]/swarm/tutorials/sorg`, because the zellij layout paths are relative to that directory. For instance, to build the infrastructure for the minimal example, run

```
./forward-data/build.sh
```

### Starting up the example infrastructure
The demo examples are run using `zellij`. For instance, to run the minimal example, run

```
zellij -l ./forward-data/layout.kdl
```

You should see that a swarm node has started (left pane -- more on this below). You also should see a pane for the interaction with the CLI, as well as two panes containing the output of the `publisher` and the `subscriber` executables. At the moment, the minimal application is not deployed, so that the subscriber does not receive any data and does not print any message.

## Swarm configuration of the sorg plugins

### Swarm config and plugins
At this point, we have a manifest describing the application we want to deploy. The next step is to specify the configuration of the swarm node(s) which we will be using. This "swarm-config" defines the types of the plugins which will be deployed on the different nodes, as well at the configuration of these plugins.

In the context of the sorg layer, the two relevant types of plugins are:

- The **orchestration plugin (orch.)** which is responsible for interacting with the sorg-client (e.g. the cli) and for coordinating the deployment and deletion of applications
- The **execution plugin (exec.)** which is responsible for starting, stopping, and monitoring the tasks which are deployed/deleted as part of deploying/deleting applications

### Configuration for the minimal example
For the minimal application example, we will use the swarm configuration described by the file `./forward-data/swarm-config.jsonnet`. With this configuration, we define a system consisting of a single swarm node, which is configured as a peer and hosts an orchestration plugin with the default configuration, as well as an execution plugin which has a configuration setting its name to "runtime".

### Deploying the swarm specified by the config
Given that you have checked out and built the `swarm` repository as described in the "Prerequisites" section, you can start up the swarm node we will use for the minimal example by running:

```
../../target/debug/swarm ./forward-data/swarm-config.jsonnet
```

Running this should start up one swarm node. The logs should say that an execution and an orchestration plugin have been loaded and started. You can close the session with `ctr + C` -- we will run the tutorial application using zellij, so you won't need to directly start the node (but zellij will run this exact command in the "runtime" pane). 

## Interacting with the sorg layer via sorg-ctl
When we ran the `build.sh` command in `forward-data` earlier, the command, among other things, copied the executable `sorg-ctl` from the main `swarm` workspace target directory into `forward-data`. When you now start zellij by running

```
zellij -l ./forward-data/layout.kdl
```

the pane on the upper right (SORG CTL) will provide access to the `./forward-data` directory, where you can interact with the CTL via the `sorg-ctl` executable. 

### Overview

In general, you can get an overview of the available commands by running:

```
./sorg-ctl --help
```

Similarly, you can get more information on a particular subcommand:

```
./sorg-ctl deployments --help
```

### Available runtimes

First, you can verify that the orchestration and the execution runtime are available. Run

```
./sorg-ctl orchestration list
```

to see the available orchestration runtimes (you should see a table with one entry, displaying the ID of the swarm node and no specific capabilities). Run

```
./sorg-ctl runtimes list
```

to see the available execution runtimes (you should see a table with one entry, displaying the same ID as the orchestration runtime, the name "runtime", and no specific capabilities).

### Initializing the application
In the next step, we will initialize the minimal example application. Initializing an application is a step where the tasks of the application are deployed onto the nodes following the mapping specified in the manifest. The tasks are initialized (the nature of the initialization depends on the specific task type -- the simple source and sink task in this example don't require a dedicated initialization step), but not yet started so that, after the initialization step, the application is ready to start, but no data is being processed yet. In general, when implementing tasks/applications, all steps which are executed once and require a long time or are fallible should, if possible, be moved into the initialization phase of the task/application.

To initialize the example application, run the command

```
./sorg-ctl deployments init ./manifest.yaml
```

The CLI output should say that the application was successfully initialized and show where the different tasks of the application have been placed (trivial in our case, since we have only one exec runtime -- both tasks will be placed there). Note also that the subscriber (pane on the bottom right) is still not receiving any messages, since the application was not yet started.

### Starting the application
In the next step, we start the application with the start command of the CLI. To this end, we need something which we can use as an identifier for the initialized deployment we want to start (since, in a running system, we could have multiple initialized deployments in different parts of the system). 

A deployment can be identifier by (a) the name of the application, as long as the name is unique among the deployments present in the system and/or (b) a prefix of the UUID of the deployment, given that you provide enough characters to uniquely identify the deployment.

The first characters of the deployment UUID and the application name are displayed in the output of the corresponding `init` command (the application name is defined in the application manifest). So, in our case, the application can be started by running

```
./sorg-ctl deployments start "forward data"
```

or by running

```
./sorg-ctl deployments start 3c3
```

NOTE: The actual UUID prefix you have to provide here will differ, since the UUID is randomly generated. The command above would work if the output of the `init` command would have had sth like `3c32b...` in the ID cell of the outcome table.

After running this command, you should (a) see CLI output indicating that the deployment was started (and that both its tasks are now running) and (b) see that the subscriber is now periodically receiving messages on the sink topic.

### Deleting the application
This step is analogous to starting the application. Here as well, you have to identify the deployment you want to delete, either by the application name or the ID prefix. After running

```
./sorg-ctl deployments delete "forward data"
```

you should (a) see CLI output indicating that the deployment has been deleted and (b) see that the subscriber (pane on the bottom right) is not receiving any messages (while the subscriber keeps sending them).

### Deploying the application
You can also use the deploy command of the CLI to initialize and start the application in one step:

```
./sorg-ctl deployments deploy manifest.yaml
```

The cli output here should be the same as it has been for the start command and the subscriber should start receiving messages again.

### Additional CLI commands
In addition to the commands described above, you can use the commands

```
./sorg-ctl runtimes status
```

to see the status of the runtimes, i.e., the deployments which are currently hosted by the accessible execution runtimes. The command

```
./sorg-ctl deployments status [deployment-identifier]
```

will provide information on the placement and status of the tasks of the deployment identified by the provided identifier (following the same logic as the identifier in the start and the delete command).

## Closing the demo
To close the demo, leave the zellij session via `ctrl + Q`.
