# Speech regression fixture

`jfk.wav` is the 11-second, 16 kHz mono, PCM16 speech example from
[whisper.cpp](https://github.com/ggml-org/whisper.cpp/tree/master/samples):
https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav

It is an excerpt from John F. Kennedy's 1961 US presidential inaugural
address (public-domain US government speech). The source project is MIT
licensed. Keep this provenance with the test fixture.

SHA-256:
`59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e`

Tests interpolate it to 48 kHz and add deterministic synthetic background
noise and short impacts. It is compiled into test binaries only, and is
not included in the distributed application. Tests never record physical
microphones or use the user's conversations.
