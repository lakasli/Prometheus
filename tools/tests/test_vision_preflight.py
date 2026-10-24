import importlib.util
import pathlib
import struct
import unittest
import zlib

spec = importlib.util.spec_from_file_location('vision_preflight', pathlib.Path(__file__).resolve().parents[1] / 'vision-preflight.py')
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class VisionFixtureTests(unittest.TestCase):
    def test_pixels_encode_the_requested_geometry_and_colors(self):
        for shape in ('circle', 'square'):
            picture = probe.png(shape, 'red', 2)
            self.assertEqual(picture[:8], b'\x89PNG\r\n\x1a\n')
            offset = 8
            compressed = bytearray()
            while offset < len(picture):
                length = struct.unpack('!I', picture[offset:offset + 4])[0]
                if picture[offset + 4:offset + 8] == b'IDAT':
                    compressed.extend(picture[offset + 8:offset + 8 + length])
                offset += 12 + length
            raw = zlib.decompress(compressed)
            def pixel(x, y):
                start = y * 721 + 1 + x * 3
                return tuple(raw[start:start + 3])
            self.assertEqual(pixel(40, 50), (240, 20, 20))
            self.assertEqual(pixel(120, 50), (240, 20, 20))
            self.assertEqual(pixel(200, 50), (255, 255, 255))
            self.assertEqual(pixel(18, 28), (255, 255, 255) if shape == 'circle' else (240, 20, 20))


if __name__ == '__main__':
    unittest.main()
