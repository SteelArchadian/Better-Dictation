"""Generates the "dictation on" cues into src-tauri/sounds/*.wav (mono, 16-bit, 22.05 kHz, no samples used).

  python -I tools/make_sounds.py
"""
import os
import wave

import numpy as np

RATE = 22050
OUT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'src-tauri', 'sounds')


def t(dur):
    return np.arange(int(RATE * dur)) / RATE


def env(n, attack=0.005, decay=6.0):
    x = np.arange(n) / RATE
    return np.minimum(1, x / attack) * np.exp(-decay * x)


def tone(freq, dur, decay=6.0, attack=0.005, harmonics=((1, 1.0),)):
    x = t(dur)
    y = sum(a * np.sin(2 * np.pi * freq * h * x) for h, a in harmonics)
    return y * env(len(x), attack, decay)


def place(parts):
    """parts: (start seconds, samples) pairs mixed into one buffer."""
    end = max(s + len(p) / RATE for s, p in parts)
    out = np.zeros(int(RATE * end) + 1)
    for s, p in parts:
        i = int(RATE * s)
        out[i:i + len(p)] += p
    return out


def note(name):
    names = {'C': 0, 'D': 2, 'E': 4, 'F': 5, 'G': 7, 'A': 9, 'B': 11}
    return 440 * 2 ** ((names[name[0]] + 12 * (int(name[1]) - 4) - 9) / 12)


def sweep(f0, f1, dur, decay=4.0):
    x = t(dur)
    phase = 2 * np.pi * (f0 * x + (f1 - f0) * x ** 2 / (2 * dur))
    return np.sin(phase) * env(len(x), 0.01, decay)


SOUNDS = {
    # soft two-note bell, rising fifth
    'chime': lambda: place([(0, tone(note('E5'), .35, 9, harmonics=((1, 1), (2, .3)))),
                            (.09, tone(note('B5'), .4, 8, harmonics=((1, 1), (2, .25))))]),
    # short single blip
    'blip': lambda: tone(880, .09, 30, .002),
    # quick upward glide
    'rise': lambda: sweep(400, 1100, .16, 9),
    # plucked string feel
    'pluck': lambda: tone(note('G4'), .3, 12, .002, ((1, 1), (2, .5), (3, .3), (4, .15))),
    # water-drop bubble: fast upward chirp, low-pass feel
    'bubble': lambda: sweep(500, 1500, .09, 25) * .9,
    # marimba: wooden short tone with an inharmonic overtone
    'marimba': lambda: place([(0, tone(note('C5'), .3, 14, .001, ((1, 1), (4, .35)))),
                              (.11, tone(note('G5'), .3, 14, .001, ((1, 1), (4, .35))))]),
    # glass tap: high, bright, long ring
    'glass': lambda: tone(note('A6'), .5, 10, .001, ((1, 1), (2.76, .35), (5.4, .12))),
    # little rising arpeggio
    'sparkle': lambda: place([(i * .06, tone(note(n), .22, 14, .002, ((1, 1), (2, .2))))
                              for i, n in enumerate(['C5', 'E5', 'G5', 'C6'])]),
    # round pop
    'pop': lambda: sweep(300, 120, .08, 35) * 1.2,
    # warm low two-tone "ready"
    'soft': lambda: place([(0, tone(note('C4'), .4, 7, .02, ((1, 1), (2, .2)))),
                           (.12, tone(note('G4'), .45, 7, .02, ((1, 1), (2, .2))))]),
}


def main():
    os.makedirs(OUT, exist_ok=True)
    for name, make in SOUNDS.items():
        y = make()
        y = y / max(1e-9, np.abs(y).max()) * 0.2  # about -14 dBFS peak: cues stay quiet
        fade = int(RATE * .01)
        y[-fade:] *= np.linspace(1, 0, fade)
        with wave.open(os.path.join(OUT, name + '.wav'), 'wb') as f:
            f.setnchannels(1)
            f.setsampwidth(2)
            f.setframerate(RATE)
            f.writeframes((y * 32767).astype('<i2').tobytes())
        print(name, f'{len(y) / RATE:.2f}s')


if __name__ == '__main__':
    main()
