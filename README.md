# Factory Cacher

Factory Cacher is a multiplayer proxy for various factory-builder games that greatly speeds
up the initial world download that occurs when a player joins the server.
It uses a technique similar to rsync to deduplicate the world data sent to the player.
It currently supports [Factorio](https://www.factorio.com/) and the
[Nebula Multiplayer Mod](https://github.com/NebulaModTeam/nebula)
for [Dyson Sphere Program](https://store.steampowered.com/app/1366540/Dyson_Sphere_Program/).

Normally when a player connects to a server in these types of games, the game client first has to download the entire game 
world from the server. Factory Cacher sits in between the game client and server and deduplicates world data sent to 
the client, allowing the server to only send the fragments of the world that have changed since the last time the user 
connected.

This program is intended to help people with slow internet connections join Factorio/DSP multiplayer servers faster.
It works best when your internet speed is below 10-20 Mb/s. If you have a 100Mb/s or higher internet speed, this 
program is unlikely to speed things up very much, or might even slow things down due to the overhead of 
deduplicating the world data.

## Building

Factory Cacher requires Rust, which can be downloaded [here](https://www.rust-lang.org/tools/install).

Factory Cacher uses a TLS certificate to encrypt its traffic. Before building the binary a certificate
must be generated and placed in the `certs` directory. The easiest way to do this is using
[rustls-cert-gen](https://github.com/rustls/rcgen/tree/main/rustls-cert-gen). Install rustls-cert-gen with
```shell
cargo install rustls-cert-gen
```
And then generate a certificate using
```shell
mkdir certs && rustls-cert-gen -o certs --san localhost
```

Finally, Factory Cacher can be built using
```shell
cargo build --release
```
The final binaries can now be found under `target/release`. Each game has its own binary.
The client and server both use the same executable.

## Using

Factory Cacher runs as two parts: the client and the server. The Cacher client runs on the same machine as the
game client, and the Cacher server runs on the same machine as the game server. A player wishing to join will
connect to their local instance of the Factory Cacher client, which will then forward the connection to the
Factory Cacher server, which will then forward the connection to the actual game server.
```
┌────────┐   ┌────────┐   ║            ║   ┌────────┐   ┌────────┐
│  Game  │<──│ Cacher │   ║  Internet  ║   │ Cacher │<──│  Game  │
│ Server │   │ Server │<──╫────────────╫───│ Client │   │ Client │
└────────┘   └────────┘   ║            ║   └────────┘   └────────┘
```

### Running - Factorio

See the [Multiplayer](https://wiki.factorio.com/Multiplayer) page on the Factorio wiki for help with setting up a
Factorio server.

First, start the Factorio server and note what port it's running on. For a dedicated server this will be 34197.
For a LAN world the port can be found in the Factorio log or set manually in the
[hidden settings](https://wiki.factorio.com/Settings#Hidden_settings).

Next, start the Factory Cacher server using
```shell
factorio-cacher server localhost:<factorio port>
```
Replace `<factorio port>` with the port of the Factorio server. The Factory Cacher server should now be running on
port 60130. The port can be changed using the `--port` option on `factorio-cacher server`.

Now the user wishing to connect to the server can start Factory Cacher using
```shell
factorio-cacher client <server IP address>:60130
```
Replace `<server IP address>` with the IP address of the machine running the Factory Cacher server. The Factorio
client should now be able to connect using `localhost:60120`. This port can be changed using the `--port` option on
`factorio-cacher client`.

### Running - Nebula Multiplayer Mod

See [this page](https://github.com/NebulaModTeam/nebula/wiki/Hosting-and-Joining) on the Nebula wiki for help with 
finding the IP and port of your server. You do not need a dedicated server, using the Host Game option on the main menu 
will work as well.

After you've started hosted your game, start the Factory Cacher server using
```shell
dsp-nebula-cacher server localhost:<nebula port>
```
Replace `<nebula port>` with the port of your Nebula server. The Factory Cacher server should now be running on
port 60130. The port can be changed using the `--port` option on `dsp-nebula-cacher server`.

Now the user wishing to connect to the server can start Factory Cacher using
```shell
dsp-nebula-cacher client <server IP address>:60130
```
Replace `<server IP address>` with the IP address of the machine running the Factory Cacher server. The DSP
client should now be able to connect using `localhost:60120`. This port can be changed using the `--port` option on
`dsp-nebula-cacher client`.

## How it works

The Factory Cacher client and server form a QUIC connection between each other. Any incoming multiplayer
connections are tunnelled over the QUIC connection. The QUIC connection is also used to exchange information between 
the Factory Cacher client and server. For Factorio, QUIC's unreliable datagram extension is used to directly tunnel 
Factorio's UDP packets. For the Nebula Multiplayer Mod, the mod's websocket connections are tunneled using a normal 
QUIC stream.

As soon as the Cacher server detects that the game server is trying to send a large chunk of data that could be 
deduplicated, it captures the data and uses a [Polynomial rolling hash](https://en.wikipedia.org/wiki/Rolling_hash#Polynomial_rolling_hash)
to split the data into chunks. Due to the rolling hash, the boundaries between chunks depend on the data itself, which 
prevents insertions or deletions from causing all chunks after the affected area from changing. Each chunk is 
then hashed with blake3 to give it a unique content-defined ID. See [this Wikipedia article](https://en.wikipedia.org/wiki/Rolling_hash#Content-based_slicing_using_a_rolling_hash)
for more information on content-based chunking. Finally, this list of chunk hashes is then sent to the Cacher client.

Once the Cacher client receives the chunk list, it starts using the list to reassemble the data.
If a particular chunk is already contained in the Cacher client's chunk cache, it'll use its cached version for
that chunk. If the client doesn't have a chunk in its cache, it'll request it from the Cacher server. Chunks are 
requested in batches to minimize round-trips across the network and the chunks are compressed with zstd before being 
sent over the network. When the Cacher client finishes reconstructing the data blob, it'll send to it on to the game
client. Special care is taken to allow reconstruction progress to show up on any progress bars/meters in the game 
itself, giving the joining player feedback that something is actually happening during the reconstruction process.

The particular type of packets that are deduplicated depends on the game. For Factorio, the initial world download 
is deduplicated. Since the game world is transmitted as a ZIP file in Factorio, the ZIP is first decompressed 
and each file is deduplicated individually. This is due to compressed data not deduplicating well.

For the Nebula Multiplayer Mod, the payloads of the `GlobalGameDataResponse`, `FactoryData`, and `DysonSphereData` 
packets are deduplicated. Only packets over ~128 KiB in size are deduplicated, as for anything smaller the 
deduplication overhead exceeds any potential space savings. 

This way only the chunks of the data that have changed since the last time the player joined will be
transmitted over the internet, significantly saving bandwidth and speeding up the player joining process. The Cacher
client periodically saves its chunk cache to a file on disk to allow the cache to be reused over many sessions.

## Credits

- Huge thanks to the [factorio-reverse](https://github.com/radioegor146/factorio-reverse/tree/master) project,
  without their repository I never could've figured out how to decode Factorio's network protocol.
- Wube Software, for making Factorio.
- The Nebula Mod Team, for making the [Nebula Multiplayer Mod](https://github.com/NebulaModTeam/nebula).
- Youthcat Games, for making [Dyson Sphere Program](https://store.steampowered.com/app/1366540/Dyson_Sphere_Program/).
