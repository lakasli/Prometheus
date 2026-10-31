"""Fixed local media engine. Runs only in the bounded Windows AppContainer job.

No subprocesses, URLs, model downloads, arbitrary paths or executable inputs.
The TypeScript broker supplies bounded input and validates every output again.
"""
import ctypes
import json
import math
import os
from pathlib import Path
import sys
import time
import wave
import io

RUNTIME = Path(__file__).resolve().parent
sys.path.insert(0, str(RUNTIME / "Lib"))
ROOT = Path(sys.argv[1]).resolve()
OUT = ROOT / "output"


def isolation():
    token = ctypes.c_void_p()
    adv = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.GetCurrentProcess.restype = ctypes.c_void_p
    if not adv.OpenProcessToken(ctypes.c_void_p(kernel.GetCurrentProcess()), 8, ctypes.byref(token)):
        raise ValueError("token")
    try:
        container = ctypes.c_uint32()
        size = ctypes.c_uint32(4)
        if not adv.GetTokenInformation(token, 29, ctypes.byref(container), 4, ctypes.byref(size)) or container.value != 1:
            raise ValueError("container")
        adv.GetTokenInformation(token, 30, None, 0, ctypes.byref(size))
        capabilities = ctypes.create_string_buffer(size.value)
        if not adv.GetTokenInformation(token, 30, capabilities, size, ctypes.byref(size)) or int.from_bytes(capabilities.raw[:4], "little") != 0:
            raise ValueError("capabilities")
        job = ctypes.c_int()
        if not kernel.IsProcessInJob(ctypes.c_void_p(kernel.GetCurrentProcess()), None, ctypes.byref(job)) or job.value != 1:
            raise ValueError("job")
        if os.environ.get("PROMETHEUS_PROBE_SECRET"):
            raise ValueError("environment")
        return {"app_container": 1, "capabilities": 0, "in_job": 1}
    finally:
        kernel.CloseHandle(token)


def audio():
    import av
    import numpy as np
    frames = []
    samples = 0
    path=ROOT/'input.bin'
    with path.open('rb') as probe:signature=probe.read(10)
    source=str(path)
    if b'#!SILK_V3' in signature:
        import pysilk
        decoded=pysilk.decode(path.read_bytes(),to_wav=True)
        if not isinstance(decoded,bytes) or len(decoded)>4*1024*1024:
            raise ValueError('silk_output_limit')
        source=io.BytesIO(decoded)
    with av.open(source, options={"protocol_whitelist": "file,pipe", "threads": "2"}) as container:
        streams = list(container.streams.audio)
        if len(streams) != 1:
            raise ValueError("audio_stream")
        stream = streams[0]
        stream.codec_context.thread_count = 2
        resampler = av.AudioResampler(format="s16", layout="mono", rate=16000)
        for frame in container.decode(stream):
            for converted in resampler.resample(frame):
                samples += converted.samples
                if samples > 960000:
                    raise ValueError("duration")
                frames.append(converted.to_ndarray().reshape(-1).astype(np.float32) / 32768)
        for converted in resampler.resample(None):
            samples += converted.samples
            if samples > 960000:
                raise ValueError("duration")
            frames.append(converted.to_ndarray().reshape(-1).astype(np.float32) / 32768)
    if not samples:
        raise ValueError("empty_audio")
    return np.concatenate(frames), samples / 16000


def recognize():
    import sherpa_onnx
    samples, seconds = audio()
    recognizer = sherpa_onnx.OfflineRecognizer.from_sense_voice(
        model=str(RUNTIME / "models/model.int8.onnx"), tokens=str(RUNTIME / "models/tokens.txt"),
        num_threads=2, provider="cpu", use_itn=True, language="auto", debug=False)
    stream = recognizer.create_stream()
    stream.accept_waveform(16000, samples)
    recognizer.decode_stream(stream)
    text = stream.result.text
    if len(text.encode("utf-8")) > 16000 or "\x00" in text:
        raise ValueError("transcript")
    # SenseVoice does not provide calibrated confidence or token timestamps here.
    return {"text": text, "seconds": seconds, "confidence": None, "timestamps": None,
            "provider": "sherpa-onnx/SenseVoice-int8", "version": sherpa_onnx.__version__}


def selftest():
    kernel=ctypes.WinDLL('kernel32',use_last_error=True)
    kernel.VirtualAlloc.restype=ctypes.c_void_p
    kernel.VirtualAlloc.argtypes=[ctypes.c_void_p,ctypes.c_size_t,ctypes.c_ulong,ctypes.c_ulong]
    kernel.VirtualFree.argtypes=[ctypes.c_void_p,ctypes.c_size_t,ctypes.c_ulong]
    small=kernel.VirtualAlloc(None,8*1024*1024,0x3000,4)
    if not small:raise ValueError('small_allocation')
    kernel.VirtualFree(small,0,0x8000)
    large=kernel.VirtualAlloc(None,2*1024*1024*1024,0x3000,4)
    if large:
        kernel.VirtualFree(large,0,0x8000)
        raise ValueError('memory_limit')
    denied={}
    for name,path,flags in [('runtime_write_denied',RUNTIME/'models/huayan.onnx',os.O_WRONLY),('input_write_denied',ROOT/'input.bin',os.O_WRONLY),('other_job_read_denied',ROOT.parent/'owner.json',os.O_RDONLY)]:
        try:
            fd=os.open(path,flags);os.close(fd);denied[name]=False
        except PermissionError:denied[name]=True
    if not all(denied.values()):raise ValueError('file_boundary')
    with (RUNTIME/'models/huayan.onnx').open('rb') as check:
        if not check.read(1):raise ValueError('model_read')
    return {'provider':'fixed-worker-selftest','python':sys.version.split()[0],'small_allocation_ok':True,'allocation_over_limit_denied':True,**denied}


def synthesize(text, encoding):
    if not isinstance(text, str) or not text.strip() or len(text) > 300 or "\x00" in text:
        raise ValueError("text")
    import onnxruntime as ort
    from piper import PiperVoice
    from piper.config import PiperConfig
    from piper.voice import ESPEAK_DATA_DIR
    options = ort.SessionOptions()
    options.intra_op_num_threads = 2
    options.inter_op_num_threads = 1
    voice = PiperVoice(config=PiperConfig.from_dict(json.loads((RUNTIME / "models/huayan.onnx.json").read_text(encoding="utf-8"))),
                       session=ort.InferenceSession(str(RUNTIME / "models/huayan.onnx"), sess_options=options, providers=["CPUExecutionProvider"]),
                       espeak_data_dir=ESPEAK_DATA_DIR, download_dir=OUT)
    with wave.open(str(OUT / "speech.wav"), "wb") as output:
        voice.synthesize_wav(text, output)
    with wave.open(str(OUT / 'speech.wav'),'rb') as check:
        if check.getnchannels()!=1 or check.getsampwidth()!=2 or check.getnframes()/check.getframerate()>60:
            raise ValueError('synthesis_duration')
    if encoding=='mp3':
        import av
        with av.open(str(OUT/'speech.wav')) as source,av.open(str(OUT/'speech.mp3'),'w',format='mp3') as target:
            stream=target.add_stream('libmp3lame',rate=22050)
            stream.layout='mono'
            stream.bit_rate=64000
            for frame in source.decode(audio=0):
                for packet in stream.encode(frame):target.mux(packet)
            for packet in stream.encode():target.mux(packet)
        (OUT/'speech.wav').unlink()
    return {"provider": "Piper", "version": "1.8.0", "voice": "zh_CN-huayan-medium"}


def video(transcribe_audio):
    import av
    times = []
    coverage = 0.0
    with av.open(str(ROOT / "input.bin"), options={"protocol_whitelist": "file,pipe", "threads": "2"}) as container:
        streams = list(container.streams.video)
        if len(streams) != 1:
            raise ValueError("video_stream")
        stream = streams[0]
        stream.codec_context.thread_count = 2
        next_at = 0.0
        count = 0
        for frame in container.decode(stream):
            count += 1
            if count > 18000 or frame.width * frame.height > 8294400 or frame.pts is None:
                raise ValueError("video_frame")
            timestamp = float(frame.pts * stream.time_base)
            if not math.isfinite(timestamp) or timestamp < 0 or timestamp > 60.1:
                raise ValueError("video_time")
            coverage = max(coverage, timestamp)
            if timestamp + 0.001 < next_at or len(times) >= 6:
                continue
            scale = min(1, 512 / max(frame.width, frame.height))
            small = frame.reformat(width=max(1, int(frame.width * scale)), height=max(1, int(frame.height * scale)), format="rgba")
            # PNG is encoded by the fixed FFmpeg library without Pillow or an external command.
            output = av.open(str(OUT / ("frame-" + str(len(times)) + ".png")), mode="w", format="image2")
            try:
                encoder = output.add_stream("png", rate=1)
                encoder.width = small.width
                encoder.height = small.height
                encoder.pix_fmt = "rgba"
                for packet in encoder.encode(small): output.mux(packet)
                for packet in encoder.encode(): output.mux(packet)
            finally:
                output.close()
            times.append(timestamp)
            next_at = timestamp + 10.0
        if not times:
            raise ValueError("empty_video")
    with av.open(str(ROOT/'input.bin')) as check:
        has_audio=bool(check.streams.audio)
    audio_result={"state":"none"} if not has_audio else {"state":"not_processed"}
    if has_audio and transcribe_audio:audio_result={"state":"transcribed",**recognize()}
    return {"provider": "PyAV/FFmpeg", "version": av.__version__, "frames_at": times,
            "decoded_until": coverage, "frame_sampling_seconds": 10, "max_frames": 6, "audio": audio_result}


try:
    started = time.monotonic()
    proof = isolation()
    if (ROOT / "request.json").stat().st_size > 8192 or (ROOT / "input.bin").stat().st_size > 20 * 1024 * 1024:
        raise ValueError("input_size")
    request = json.loads((ROOT / "request.json").read_text(encoding="utf-8"))
    kind = request.get("kind")
    if kind == "tts": result = synthesize(request.get("text"),request.get('encoding','wav'))
    elif kind == "asr": result = recognize()
    elif kind == "video": result = video(request.get('transcribe_audio') is True)
    elif kind == "selftest": result = selftest()
    else: raise ValueError("operation")
    receipt = {"status": "completed", "kind": kind, "isolation": proof, "result": result, "elapsed_ms": round((time.monotonic() - started) * 1000)}
except BaseException as error:
    # Never persist an engine traceback, input text, filesystem path or model output on failure.
    receipt = {"status": "failed", "code": "media_engine_failed", "error_type": type(error).__name__}
(OUT / "result.json").write_text(json.dumps(receipt, ensure_ascii=False), encoding="utf-8")
