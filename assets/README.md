# Voice denoising model

`voice.rnn` is the compact **beguiling-drafter-2018-08-30** model from
[GregorR/rnnoise-models](https://github.com/GregorR/rnnoise-models).
The author's model table targets human voice (including non-speech sounds
such as laughter) against recording noise. The repository README declares
the model data to be outside copyright; see its notice for the distinction
between the data and the tools.

Source text weights:
https://raw.githubusercontent.com/GregorR/rnnoise-models/master/beguiling-drafter-2018-08-30/bd.rnnn

Conversion uses the `train/convert_rnnoise.py` script distributed with
`nnnoiseless` 0.5.2: discard the format header and encode each whitespace
separated integer as its two's-complement byte (`int(value) % 256`).
The 87,521-byte result is embedded in the client; no runtime download or
external model path is required. Validate its exact size with `wc -c`.

SHA-256:
`5d059602d7dfeca9c27cf50eb6439b7f7c8f5e2588b9b4c544ec10e658d3382e`

The inference implementation is the BSD-3-Clause-licensed
[`nnnoiseless`](https://github.com/jneem/nnnoiseless) 0.5.2 crate, a Rust
implementation derived from Xiph RNNoise. The crate's default command line,
training and audio-file features are disabled in this project.
