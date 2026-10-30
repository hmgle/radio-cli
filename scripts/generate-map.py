#!/usr/bin/env -S uv run --script
"""Generate the bundled Natural Earth land rings; no runtime GIS dependency."""
import hashlib
import json
import pathlib
import struct
import urllib.request

SOURCE = 'https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_110m_land.geojson'
raw = urllib.request.urlopen(SOURCE, timeout=60).read()
assert hashlib.sha256(raw).hexdigest() == '9e0729ee253ca7d7a5c4ae9395fb1902264c5377c52e224d13dd85010e2835d9', 'source checksum changed'
features = json.loads(raw)['features']
rings = []
for feature in features:
    geometry = feature['geometry']
    polygons = [geometry['coordinates']] if geometry['type'] == 'Polygon' else geometry['coordinates']
    for polygon in polygons:
        for ring in polygon:
            rings.append([(round(lon * 1_000_000), round(lat * 1_000_000)) for lon, lat, *_ in ring])
output = bytearray()
for ring in rings:
    output.extend(struct.pack('<I', len(ring)))
    for point in ring:
        output.extend(struct.pack('<ii', *point))
root = pathlib.Path(__file__).resolve().parents[1]
(root / 'src/tui/map/land.bin').write_bytes(output)
print(f'{SOURCE}\nSHA256 {hashlib.sha256(raw).hexdigest()}\n{len(rings)} rings, {sum(map(len,rings))} vertices, {len(output)} bytes')
