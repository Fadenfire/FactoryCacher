# Factorio Cacher

Factorio Cacher is a proxy for [Factorio](https://www.factorio.com/) multiplayer connections that greatly speeds
up the initial world download using a technique similar to rsync.

Normally when a player connects to a Factorio server, the Factorio client first has to download the entire game 
world from the server. Factorio Cacher sits in between the Factorio client and server and caches the world data sent to 
the client, allowing the server to only send the fragments of the world that have changed since the last time the user 
connected.

## Building

Factorio Cacher requires Rust, which can be downloaded [here](https://www.rust-lang.org/tools/install).

Factorio Cacher uses a TLS certificate to encrypt its traffic. Before building the binary a certificate
must be generated and placed in the `certs` directory. The easiest way to do this is using
[rustls-cert-gen](https://github.com/rustls/rcgen/tree/main/rustls-cert-gen). Install rustls-cert-gen with
```shell
cargo install rustls-cert-gen
```
And then generate a certificate using
```shell
mkdir certs && rustls-cert-gen -o certs --san localhost
```

Finally, Factorio Cacher can be built using
```shell
cargo build --release
```
The final binary can now be found under `target/release`. The client and server both use the same executable.

## Using

Factorio Cacher runs as two parts: the client and the server. The Cacher client runs on the same machine as the
Factorio client, and the Cacher server runs on the same machine as the Factorio server. A player wishing to join will
connect to their local instance of the Factorio Cacher client, which will then forward the connection to the
Factorio Cacher server, which will then forward the connection to the actual Factorio server.
```
┌────────┐   ┌────────┐   ║            ║   ┌────────┐   ┌────────┐
│Factorio│←──│ Cacher │   ║  Internet  ║   │ Cacher │←──│Factorio│
│ Server │   │ Server │←──╫────────────╫───│ Client │   │ Client │
└────────┘   └────────┘   ║            ║   └────────┘   └────────┘
```

### Running

See the [Multiplayer](https://wiki.factorio.com/Multiplayer) page on the Factorio wiki for help with setting up a
Factorio server.

First, start the Factorio server and note what port it's running on. For a dedicated server this will be 34197.
For a LAN world the port can be found in the Factorio log.

Next, start the Factorio Cacher server using
```shell
factorio-cacher server localhost:<factorio port>
```
Replace `<factorio port>` with the port of the Factorio server. The Factorio Cacher server should now be running on
port 60130. The port can be changed using the `--port` option on `factorio-cacher server`.

Now the user wishing to connect to the server can start Factorio Cacher using
```shell
factorio-cacher client <server IP address>:60130
```
Replace `<server IP address>` with the IP address of the machine running the Factorio Cacher server. The Factorio
client should now be able to connect using `localhost:60120`. This port can be changed using the `--port` option on
`factorio-cacher client`.

## How it works

The Factorio Cacher client and server form a QUIC connection between each other. Any incoming Factorio multiplayer
connections are tunnelled over the QUIC connection using QUIC's unreliable datagram extension. When a new Factorio
connection is established, the Cacher server will wait until it receives the `MapReadyForDownload` packet from the
Factorio server.

As soon as the Cacher server receives `MapReadyForDownload`, it'll download the game world from the Factorio server.
It then unzips the download world ZIP file and uses a
[Polynomial rolling hash](https://en.wikipedia.org/wiki/Rolling_hash#Polynomial_rolling_hash) to split each contained
file into chunks. Due to the rolling hash, the boundaries between chunks depend on the data itself, which prevents
insertions or deletions from causing all chunks after the affected area from changing. Each chunk is hashed with
blake3 to give it a unique content-defined ID. See [this Wikipedia article](https://en.wikipedia.org/wiki/Rolling_hash#Content-based_slicing_using_a_rolling_hash)
for more information on content-based chunking. Finally, this list of chunk hashes for each file (along with various 
other world metadata) is then sent to the Cacher client.

Once the Cacher client receives the chunk list, it starts using the list to reassemble the world ZIP on its side.
If a particular chunk is already contained in the Cacher client's chunk cache, it'll use its cached version for
that chunk. If the client doesn't have a chunk in its cache, it'll request it from the Cacher server. When the
Cacher client finishes reconstructing the world ZIP, it'll send its own `MapReadyForDownload` packet to the waiting
Factorio game client. The Factorio client will then begin downloading the world ZIP from the Cacher client's locally 
reconstructed copy.

This way only the chunks of the world data that have changed since the last time the player joined will be 
transmitted over the internet, significantly saving bandwidth and speeding up the player joining process. The Cacher 
client periodically saves its chunk cache to a file on disk to allow the cache to be reused over many sessions.

## Credits

- Huge thanks to the [factorio-reverse](https://github.com/radioegor146/factorio-reverse/tree/master) project,
  without their repository I never could've figured out how to decode Factorio's UDP based network protocol.
- Wube Software, for making Factorio
