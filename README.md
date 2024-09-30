# net-relay
A high performence Socks5 proxy server with bind/reverse support

# Features

* Async
* Single executable
* Linux/Windows/Mac/BSD support
* Support reverse mode(Not bind any port in client)

# Build & Run
Build master-node and run in central server.

`$> cd master-node`

`$> cargo build --release`

Build slave-node and run in client devices.

    slave-node/java : NetBeans java project. (Gradle 8.7, Java 17)


## Bind Mode

You can run a socks5 proxy and listen port at 1080

`$> ./net-relay -l 0.0.0.0:1080`

## Reverse Mode

First listen a port waiting for slave connection

`$> ./net-relay -t 0.0.0.0:8000 -d 0.0.0.0:8001 -s 0.0.0.0:1080`

then reverse connect to master in slave

`$> ./net-relay -r 127.0.0.1:8000`

or

run APK.