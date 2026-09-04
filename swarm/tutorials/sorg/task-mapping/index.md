# Task mapping - controlling task placement
In this third tutorial, we will be detailing the different ways to specify how the sorg layer shall place tasks in the cases where there are multiple available swarm nodes and/or tasks with specific requirements towards host nodes. The tutorial assumes that you are already familiar with the previous two tutorials. In this next step, we will be deploying the application from the second tutorial to see the different ways to specify the task mapping.

## Overview
At the moment of writing, the sorg-layer supports three types of defining how the tasks of the deployed application will be placed onto the available swarm nodes: Unspecified, Fix, and Tag-based. This section of the tutorial explains the three modes and demonstrates their effect by deploying the application from the second tutorial step in a system with 3 available swarm nodes.

## Swarm configurations
The three examples presented in this tutorial all use the swarm configurations given by the files `./task-mapping/runtime_one.jsonnet`, `./task-mapping/runtime_two.jsonnet`, and `./task-mapping/runtime_three.jsonnet` (note: in this case, we could have used a single `.jsonnet` file to define the config of the 3 nodes, but having 3 files (a) is more similar to a situation where we deploy the swarm nodes on distributed machines and (b) this lets us have separate panes for the different nodes in the zellij session). Looking at the configurations, you will notice that:

- we are now deploying 3 zenoh nodes
- all of them host an execution runtime
- 2 of them host an orchestration runtime (we need at least one, 3 would also be fine, a random orchestration runtime is used for every interaction)
- one of them is hosting a filestore (at the time of writing, the assumption is that there is exactly one filestore plugin in the system, the current filestore being a temporary solution)
- the execution runtimes are tagged

## Running the 3 examples
In this tutorial, each of the three mapping types is specified via a separate application manifest. Apart from the mapping section, the manifest files are identical and describe the `even_filter` application used for the second tutorial. We are also using the same `publisher` and `subscriber` executables to produce/visualize the input and output of the application. As with the other tutorials, these binaries are built through the main `swarm` workspace and end up under `../../target`.

As before, the first step is building the necessary binaries by running:

```
./task-mapping/build.sh
```

Also as before, we use `zellij` to coordinate the start of the runtimes and executables:

```
zellij -l task-mapping/layout.kdl
```

to then use the `sorg-ctl` to deploy the applications and interact with them. 


## Unspecified mapping
As you have likely noted during the previous tutorials, specifying a mapping for the application tasks is optional. If not mapping directives are provided, the application will be deployed in a way minimizing the number of used nodes. You can verify it by starting a zellij session and deploying the application using:

```
./sorg-ctl dp deploy unspecified_mapping.yaml
```

From the output of the CLI (or by querying the runtimes' status via `./sorg-ctl rt status`), you should see that all tasks have been deployed on a single runtime (the choice between the tree available runtimes being random).

## Fix mapping
The second option to define the mapping of a task is to provide the name of the execution runtime that it shall be placed on. The file `./task-mapping/fix_mapping.yaml` illustrates how tasks can be fixed on a runtime. Note that (a) the mapping is specified on the task- not the application- level and (b) that you use different mapping types for different tasks of the application (in this example, we are using a fixed mapping for two tasks and don't specify the mapping for the third one). Again, you can run the example by starting a zellij session and deploying the application via:

```
./sorg-ctl dp deploy fix_mapping.yaml
```

From the output, you should see that the source task is deployed on runtime 1 and the operator is deployed on runtime 3, as defined in the mapping section of the manifest. Since we have not defined a mapping for the sink task, the system will minimize the number of used nodes by placing it on either runtime 1 or runtime 3.

Note: You may have noticed that the number of tasks has increased. The sender and receiver tasks (which we have not defined in the application manifest) implement the inter-runtime communication and are displayed just for the sake of completeness/debugging purposes.

## Tag-based mapping
The last available option is a mapping based on task- and runtime-tags. Hereby, task tags represent the requirements that the task has regarding a host runtime, while the runtime tags represent the capabilities of an execution runtime. Placing a tagged task on a runtime is only allowed if the runtime is tagged with all the tags that the task has as its requirements (a runtime offering more capabilities, i.e., additional tags, is also valid). Task tags are assigned in the mapping section of the application manifest while runtime tags are part of the configuration of execution runtimes and are specified in the corresponding swarm config file. In principle, tags can be used to express anything from user preference for placement to the requiremenet/availability of specific resources, e.g., GPUs. The example illustrating tag-based mapping is run by starting the zellij session and deploying the application via:

```
./sorg-ctl dp deploy tag_based_mapping.yaml
```

In this case, we expect the source and sink task to be deployed on runtime 1 (with both runtime 1 and runtime 2 fulfilling the tag requirements of the sink task, we will be minimizing the number of nodes by placing it on runtime 1) and the operator to be deployed on runtime 2.
