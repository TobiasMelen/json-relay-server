# json-relay-server

This is a simple server application to relay json messages between websockets, server sent events and http. Useful for client to client, or webhook to client purposes. Can be used in any one-to-one,one-to-many, many-to-many constellations. Written in rust using warp as server and tokio for message broadcasting.
You probably should not use it directly, but feel free to copy parts of the [code](/src/main.rs)

## Why
I seem to always require a general way for client apps to two-way communicate or listen, be it webrtc signaling, reacting to CMS updates from webhooks etc. Finding a cheap and easy way to host WS in todays PAAS cloud is suprisingly difficult. Having a simple binary copy-pasta to support 90% of the use cases is handy.

## Usage
Clients can connect to "subjects" to listen for messages. A subject can have an arbitrary number of listeners. Listeners can connect in the following ways.
- **Websockets:** connect by websocket to path `/ws/{SUBJECT}`.
- **Server sent events:** connect by [EventSource](https://developer.mozilla.org/en-US/docs/Web/API/EventSource) to `/sse/{SUBJECT}`

Sending messages to other clients can be done in the following ways
- **Websockets:** connect by listening to a subject and send a text message containing json with the property to `{to: "{SUBJECT}"}`, where `{SUBJECT}` is the subject where the message should be sent. To support two way communication, the `to` property will be replaced by a `from` property with the senders subject on the receiving end.
- **Http:** send a POST request with json body to `/http/{SUBJECT}`. The body will be forwarded to any listeners of this subject. Http sends are not listeners and no `from` property will be added.

## Security considerations
A server like this can be abused in novel ways. Consider what security risks exists for your usage. DO NOT let an attacker DOS your end users. Generally:
- Use random, non guessable subject names where possible.
- Don't send sensitive data, and don't trust data that is sent.