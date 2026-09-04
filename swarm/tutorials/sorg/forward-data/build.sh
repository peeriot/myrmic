#!/bin/bash

START_DIR="$(pwd)"
SCRIPT_DIR="$(dirname "$(readlink -f "$0")")"
SWARM_DIR="$SCRIPT_DIR/../../.."

finish(){
    cd $START_DIR
}

trap finish EXIT

printf "building the swarm repo\n\n"
cd $SWARM_DIR
cargo build 

printf "copying the cli binary\n\n"
cd $SCRIPT_DIR
cp $SWARM_DIR/target/debug/sorg-ctl .

printf "building the demo executables\n\n"
cd $SWARM_DIR
cargo build -p publisher
cargo build -p subscriber

