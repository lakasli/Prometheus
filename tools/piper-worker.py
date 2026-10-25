#!/usr/bin/env python3
"""One private Piper request per process. Embedded in the Rust executable.

stdin is bounded JSON; stdout is bounded PCM WAV only. No HTTP service, shell,
download, inherited API credentials, or resident worker is needed. The selected
espeak huayan model uses Piper's bundled phonemizer and needs no cloud access.
"""
import io
import json
import os
import pathlib
import sys
import wave


def main():
    if os.name == 'posix':
        import resource
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        resource.setrlimit(resource.RLIMIT_CPU, (45, 45))
        # Inference guard, not a security sandbox for arbitrary code.
        resource.setrlimit(resource.RLIMIT_AS, (1536 * 1024 ** 2, 1536 * 1024 ** 2))
    raw = sys.stdin.buffer.read(8193)
    if len(raw) > 8192:
        raise ValueError('Input too large')
    request = json.loads(raw)
    if set(request) != {'text', 'model'}:
        raise ValueError('Unexpected input')
    text, model = request['text'], pathlib.Path(request['model'])
    if not isinstance(text, str) or not 0 < len(text) <= 400:
        raise ValueError('Invalid text')
    import onnxruntime
    from piper import PiperVoice
    from piper.config import PiperConfig
    from piper.phonemize_espeak import ESPEAK_DATA_DIR
    options = onnxruntime.SessionOptions()
    options.intra_op_num_threads = 2
    options.inter_op_num_threads = 1
    options.execution_mode = onnxruntime.ExecutionMode.ORT_SEQUENTIAL
    options.log_severity_level = 3
    config = json.loads(model.with_suffix(model.suffix + '.json').read_text(encoding='utf-8'))
    if config.get('phoneme_type') != 'espeak' or config.get('espeak', {}).get('voice') != 'cmn':
        raise ValueError('Unexpected voice configuration')
    voice = PiperVoice(
        config=PiperConfig.from_dict(config),
        session=onnxruntime.InferenceSession(str(model), sess_options=options, providers=['CPUExecutionProvider']),
        espeak_data_dir=ESPEAK_DATA_DIR,
        download_dir=model.parent,
    )
    output = io.BytesIO()
    frames = 0
    with wave.open(output, 'wb') as wav:
        wav.setframerate(22050)
        wav.setsampwidth(2)
        wav.setnchannels(1)
        for chunk in voice.synthesize(text):
            if (chunk.sample_rate, chunk.sample_width, chunk.sample_channels) != (22050, 2, 1):
                raise ValueError('Unexpected audio format')
            data = chunk.audio_int16_bytes
            frames += len(data) // 2
            if frames > 22050 * 90:
                raise ValueError('Audio duration limit')
            wav.writeframes(data)
    if frames <= 0:
        raise ValueError('No audio')
    sys.stdout.buffer.write(output.getvalue())


if __name__ == '__main__':
    try:
        main()
    except Exception:
        # Caller records a stable error code; raw text/paths never enter logs.
        sys.stderr.write('piper_worker_failed\n')
        sys.exit(1)
