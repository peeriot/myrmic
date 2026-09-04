# Swarm Onboarding Protocol

## Purpose

The purpose of the protocol is to serve as the vehicle for onboarding Swarm nodes to the Swarm network.

## Overview

The protocol is always carried out between two entities (peers):
- Device - this is the peer that needs to be onboarded to the Swarm network;
- Installer - this is the peer that carries out the onboarding process by providing onboarding data to the Device.

## Goals

### Confidentiality

Except for the initial key exchange carried out via the first two messages of the Onboarding protocol (see below), the rest of the protocol is carried out with messages being encrypted so that no sensitive data can be eavesdropped by a malicious third party.

The messages are encrypted using a symmetric key which is obtained using a [Diffie-Hellman](https://en.wikipedia.org/wiki/Diffie%E2%80%93Hellman_key_exchange) key derivation, where both peers are exchanging their EC public keys with the first two messages of the Onboarding protocol.

The key derivation is based on the [ECIES](https://fiveable.me/elliptic-curves/unit-3/elliptic-curve-integrated-encryption-scheme-ecies/study-guide/ld7deLPycGFCjeeR) algorithm that utilizes elliptic curives.

One deviation from the ECIES protocol is that rather than a single message exchanged between the two peers, multiple messages are being exchanged, and furthermore and as per below, each exchanged message is in turn chunked into 512 byte sequences where each sequence is encrypted and tagged separately. However, this difference is considered non-essential.

### Authenticity

All encrypted messages are signed by each party. This is an outcome from using a symmetric crypto which MUST be capable of generating a digest for each packet exchanged between the two parties. In other words, the symmetric crypto MUST preserve authenticity and support integrity.

Furthermore, the Swarm Onboarding protocol provides pluggable means for the Device to provide a proof-of-authenticity (or rather, attestation) to the Installer so that the Installer can verify that the Device is an authentic, genuine one. 

One way to do that is to provide signed certificates attesting that the public key used by the device in the Diffie-Hellman EC key derivation is signed by a Certificate Authority, which certificates the Installer could validate. But with that said, this is left open and depends completely on the actual payload of the DeviceAttestation message to be utilized by the user in the concrete instantiation of the Onboarding protocol (see Pluggability below).

### Pluggability

Two aspects of the onboarding protocol are open to user-specific (or network-specific) implementations:
- The format and content of the Device attestation sent by the Device to the Installer (message DeviceAttestation, see below)
- The format and content of the onboarding data sent by the Installer to the Device (message OnboardingData, see below)

### Independence from the underlying transport

While the reality might be that - most often than not - the messages of the protocol will be exchanged over the Zenoh transport, there is nothing in the onboarding protocol that requires Zenoh to be used as the underlying transport.

### Onboarding vs Operational transport

The Onboarding transport is the transport used to exchange all messages (except the first one - see below) of the Swarm Onboarding protocol.

The Operational transport is the transport that the Device would use _after_ being successfully onboarded. Most often than not, the Operational transport would be Swarm/Zenoh which - in turn - is layered on top of a lower-level protocol like TCP/IP, BLE or other.

Therefore, the primary purpose of the Swarm Onboarding protocol is to supply the onboarded Device with credentials for the Operational transport, as well as for any necessary credentials for the (link-layer) transport which is _below_ the Operational transport (like, Wifi credentials if the Device intents to connect to the Operational transport over Wifi).

Other than being the "target" of the onboarding process, the Operational transport is not utilized by neither the Installer nor the Device for the purposes of the onboarding process.

### Onboarding transport - requirements

Regardless whether the Onboarding protocol is layered on top of Zenoh, or - say - on top of plain TCP/IP, the Onboarding transport - in all of its layers - by necessity should _not_ require any networking credentials from the Device, as the Device can't know those in advance before being onboarded. Or if network credentials are required, these should be hard-coded in the Device firmware. Therefore, certain network protocols are not as suitable for use by the Onboarding protocol. For example:
- Wifi protected with WEP or WPA is not a suitable transport for the Onboarding protocol, as the Device cannot know the Wifi credentials before being onboarded first;
- Thread (because the Thread Dataset needs to be known in advance);
- BLE with encryption.

Link-layer protocols suitable for onboarding:
- BLE without encryption;
- Ethernet (+ TCP/IP on top).

Link-layer protocols whose suitability for onboarding needs to be investigated:
- Wifi action frames;
- LTE (+ TCP/IP on top);
- Open Wifi networks (+ TCP/IP on top).

#### Discoverability

For the Device and Installer to initiate a connection on the Onboarding transport level, they need to discover each other first. Informally speaking, if BLE is used as the Onboarding tranport, the peer initiating the connection needs to know the BLE MAC of the other peer. Or if TCP/IP + Ethernet is used, the initiating peer needs to know the IP address of the other peer.

While currently the discoverability aspect is out of scope for this document, check the Appendix for possible approaches towards discoverability.

### Protocol details

The protocol consists of the following 5 message types:
- OnboardedDeviceInfo
- InstallerMeta
- DeviceAttestation
- OnboardingData
- DeviceOnboardingStatus

NOTE:
- The first two messages, OnboardDeviceInfo and InstallerMeta are send unencrypted
- All other messages are sent encrypted

### Protocol message exchange at a glance

```
Device                                             Installer
(out-of-band, unencrypted, via QR/NFC/BLE etc.)
(send)    -> OnboardedDeviceInfo                   ->  (receive)

(in-band, unencrypted)
(receive) <- InstallerMeta                         <-  (send)

(in-band, encrypted)
(send)    -> DeviceAttestation                     ->  (receive)
(receive) <- OnboardingBundle                      <-  (send)
(send)    -> DeviceOnboardingStatus (done = false) ->  (receive)
...
(send)    -> DeviceOnboardingStatus (done = true)  ->  (receive)
```

#### Unencrypted portion of the protocol

##### OnboardedDeviceInfo

This is the first exchanged message.
The message is generated by the Device and is consumed by the Installer.

Unlike all other messages, this message _does_ need to be exchanged "out of band", using QR or NFC and NOT via the Onboarding transport. 

Using QR or NFC does require - by necessity - that the human individual operating the Installer software is in close proximity to the Device or to the package box of the Device. This - in turn - is used as a "proof of possession" of the Device by the human individual - or the legal entity represented by that human individual - and therefore, as an allowance to that human individual to onboard the device in the Swarm network to which the Installer software is connected and has credentials for.

Should the OnboardedDeviceInfo message be exchanged "in-band" via the Onboarding transport, there is a danger that another party which is eavesdropping the initial, unencrypted messages of the Onboarding protocol might be able to connect and onboard the Device prior to the actual owner of the device.

###### Message data

- An elliptic-curve public key which does belong to the Device, and which - since the OnboardDeviceInfo message is to be encoded in a QR or NFC - is static in nature;
  - The public key should have a one-byte prefix whose upper 4 bits are designated for the **elliptic key type** of the public key of the Device
    - Currently, only 0 is supported which designates NistP256 for the elliptic curve
  - The lower 4 bits are designated for the symmetric key to be derived using the EC Diffie-Hellman key derivation
    - Currently, only 0 is supported which designates AES-GCM-256 as the symmetric key type
  - The length of the public key depends on the type of the public key, as described by the byte prefix
- A Device profile 
  - Currently, the payload of the Device profile is out of scope for this document
  - Elaborations on possible payloads for the Device profile are available in the Appendix section

##### InstallerMeta

This is the second exchanged message, and the first message sent via the Onboarding protocol.

(**TBD**: think what that would mean if the Onboarding protocol is layered over - say - TCP/IP without Zenoh; in that case the Device would be initiating the TCP/IP connection yet it wouldn't be sending anything - the Installer would; that might be OK, but needs to be double-checked).

The message is generated by the Installer and is consumed by the Device.

###### Message data

- A NistP256 EC public key (65 bytes) that belongs to the Installer;
 - This public key (and its corresponding EC private key) **MUST** be ephemeral. In other words, the Installer **MUST** generate a new key-pair using a crypto random source of data each time it receives an OnboardedDeviceInfo message, where the generated key-pair is **NEW** for each Onboarding protocol message exchange.

The message is in JSON format with the following structure:

```json
{
    "dh_installer_pub_key": "(string)"
}
```

...where `dh_installer_pub_key` is the _ephemeral_ NistP256 public key of the installer (65 bytes), in a BASE64-standard-encoded format.

A JSON-format is selected (rather than the message only containing the Installer public key) so that there is a possibility for extensibility of the message in future, with additional fields and/or deprecated fields.

#### Encrypted portion of the protocol

Once the MessageMeta message is received by the Device, what had effectively happened is that the Device and the Installer had exchanged their NistP256 EC public keys:
- The Device had sent its static (by nature) public key to the Installer via the OnboardedDeviceInfo out-of-band message;
- The Installer had sent its dynamic/ephemeral public key to the Device via the InstallerMeta message.

Therefore, each peer can now perform a Diffie-Hellman key derivation to obtain the AES-GCM-256 key that would secure the rest of the exchanged messages. For the derivation, HKDF-SHA256 is used.

The encryption of all subsequent messages with the derived AES-GCM-256 key must be performed as follows, for each message being sent:
- The message binary payload is split into chunks of 512 bytes, where the last sequence could be smaller than 512 bytes;
- Each such chunk is encrypted as follows:
  - AES-GCM-256 nonce - 12 bytes;
  - AES-GCM-256 tag - 16 bytes;
  - chunk len - 2 bytes in LE;
  - chunk - variable length but up to 512 bytes - encrypted.

The chunking of each sent message is done so that there is no requirement for the peers to be capable of loading the complete message in-memory before decrypting. This might be important especially for the Device peer, when it is having limited hardware resources (by the virtue of potentially being a  micro-controller).

Note also that - as of the time of writing this document - there is no "streaming" Zenoh implementation in existence - even for constrained devices. 
Upon receival, all Zenoh implementations currently **DO** load the complete network message in-memory before delegating it up to the application layer. Similarly - upon sending - all Zenoh implementations expect the message to be sent to be available in its completeness in-memory.

Therefore, the chunking is more of a future-proofing rather than an actual solution that can be applied when the Onboarding protocol is layered on top of Zenoh ATM.

##### DeviceAttestation

This is the third exchanged message.

The message is generated by the Device and is consumed by the Installer.

###### Message data

The data and format of the message is completely open and subject to further specializations of the Swarm Onboarding protocol.

This message exists so that the Device could send to the Installer additional data, which cannot be encoded in the out-of-band OnboardedDeviceInfo messages due to size constraints, whereas if that data were to be encoded in the OnboardedDeviceInfo message, that would overflow the QR code size, or the NFC message size, or the BLE advertisement size which are used as the carriers of the OnboardedDeviceInfo message.

Most often than not, the DeviceAttestation message - as its name suggests - is expected to contain X509 certificates or other credentials that can serve as a proof that the Device is an authentic one, where these credentials match the static public key which is sent by the device using the OnboardedDeviceInfo message.

However, this is left completely open, and one possible extreme could be that the message is empty (i.e. it contains 0 bytes).

**TBD IMPORTANT!!!**: In case the Device does send X509 creds matching its public key as communicated in the OnboardedDeviceInfo message, do we need an explicit step where the Installer is asking the Device to _sign_ something with the corresponding private key? I would say, rather not, as the whole encrypted part of the protocol is in fact containing signatures (the AES-GCM-256 tags) which are generated using the derived AES-GCM-256 key?

##### OnboardingData

This is the fourth exchanged message.

The message is generated by the Installer and consumed by the Device.

###### Message data

The data and format of the message is completely open and subject to further specializations of the Swarm Onboarding protocol.

This message exists so that the Installer could send the Onboarding data to the Device.

However, the format and the content of this message is _not_ defined in the Onboarding protocol and is subject to further specialization of it.

##### DeviceOnboardingStatus

This is the fifth exchanged message.

The message is generated by the Device and consumed by the Installer.

Rather than one single message instance as all others so far, this message represents a sequence of 1 or more messages, so that the Installer can receive a progress information from the Device w.r.t. its processing of the OnboardingData message. In other words, sending the OnboardingData and receiving the DeviceOnboardingStatus messages might potentially even _overlap_, when a true streaming transport is used underneath the Onboarding protocol.

###### Message data

The message is in JSON format with the following structure:

```json
{
    "status": "(string)",
    "done": false
}
```

...where `status` is a human-readable status message, and `done` is indicating whether the Device had completed the onboarding process.

It is expected that the Device would send at least one message, where that message _should_ contain `done = true`, and that should be the final message in the sequence.

A JSON-format is selected (rather than the message only containing a boolean flag) so that a status message can be communicated as well, and so that there is a possibility for extensibility of the message in future, with additional fields and/or deprecated fields.

## Appendix

This section contains information which is _suggestive_ rather than prescriptive in nature.
It covers possible approaches to those aspects of the Oboarding protocol which are currently considered out of scope for the protocol. Some of this information might graduate to being prescriptive over time, and as such might be moved injto the main sections of the document.

### (Device) Discoverability

Below two possible approaches how the Installer and the Device might discover each other - for BLE and TCP/IP-over-Ethernet.

#### BLE

A possible discoverability algorithm for BLE might work as follows:
- The Installer would be the peer that initiates the connection to the Device - and therefore - needs to discover the Device;
- The Device is operating as a BLE peripheral and is sending advertisements while in Onboarding mode;
- Each advertisement contains a **discriminator** - a unique 16 bit key which is available and known to the installer as part of the out-of-band OnboardedDeviceInfo message that the Installer needs to receive before the Discoverability process starts (see below);
- Each advertisement also contains a BLE Service GUID unique to the Swarm Onboarding Protocol that identifies the BLE advertisement as a one issued by a Device that seeks to be onboarded;
- The Installer is actively scanning for BLE advertisements containing the discriminator from the OnboardedDeviceInfo message and once it detects such an advertisements, it takes the role of a BLE central and connects to the BLE peripheral that issues the advertisement.

#### Ethernet

The mDNS protocol can be used when the Device is connected to Ethernet and a valid IP network prior to the Onboarding process initiation.
Specifically, mDNS discoverability might work as follows:
- The Device is actively advertising itself over mDNS as an mDNS Service with the following properties:
  - service type: _swarmo._tcp (_swarmo stands for Swarm Onboarding)
  - service sub-type: D<discriminator> (where the discriminator is stringified as a decimal number)
  - service TCP port: a key-value pair in the _TXT payload of the mDNS message with a key = "port" and value = a stringified 16 bit decimal designating the port where the Device would listen - over plain TCP/IP - for the Onboarding protocol messages

#### Zenoh + TCP/IP + Ethernet and Zenoh + BLE

A variation of the above two schemes for BLE and Ethernet could be used when the Onboarding protocol needs to be carried over on top of Zenoh.

### Device Profile

The Device profile payload would be a convenient vehicle to communicate the following device-specific details:
- The transport or transports supported by the Device as the Swarm Onboarding tranport. E.g. BLE and/or Ethernet+TCP/IP; possibly others in future
  - This information might be crucial for the Installer so that it can discover the device via e.g. BLE advertisements or mDNS
- The link-layer transport or transports supported by the Device as the Swarm Operational transport. E.g. Wifi, Ethernet, BLE and others
  - This information might be helpful for the Installer so that it includes link-layer credentials in the OnboardingBundle
- The device type (e.g. Cortex-A with Embedded Lunux, Espressif esp32 MCU etc.)

Of the three informational aspects, the only one that absolutely has to be in the Device profile is the Swarm onboarding transports supported by the Device.
The other two might just as well be part of the DeviceAttestation message.

### Layering of the Onboarding protocol on top of Zenoh

If the Onboarding protocol is carried over on top of Zenoh, the following topics could be used:

- `/onboarding/{device-pub-key}/meta`
  - This is a topic where the first in-band message (not counting the OnboardedDeviceInfo one) - InstallerMeta - should be set by the Installer by using the Zenoh `set` verb;
- `/onboarding/{device-pub-key}/att`
  - This is a topic where the second in-band message - DeviceAttestation - should be set by the Device by using the Zenoh `set` verb;
- `/onboarding/{device-pub-key}/data`
  - This is a topic where the third in-band message - OnboardingData - shuould be set by the Installer by using the Zenoh `set` verb;
- `/onboarding/{device-pub-key}/status`
  - This is a topic where the fourth (and subsequent) in-band messages - OnboardingStatus - should be published by the Device by using a Zenoh `publish` verb.

In all of those topics, `{device-pub-key}` designates the Base64-encoded NistP256 public key of the Device, as communicated out-of-band using the OnboardedDeviceInfo message. So in a way, the Device public key has a dual purpose:
- It is used for the EC Diffie-Hellman key exchange;
- It is used to derive a unique name for the Zenoh topics where the Installer and the Device would communicate.
