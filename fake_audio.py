import numpy as np
import math


class FakeAudioEngine:
    def __init__(self, num_bars=32):
        self.num_bars = num_bars
        self.fft_bars = np.zeros(num_bars)
        self.frame    = 0

    def update(self):
        self.frame += 1
        t    = self.frame * 0.04
        bars = np.zeros(self.num_bars)
        for i in range(self.num_bars):
            pos  = i / self.num_bars
            bass = 0.8  * math.exp(-((pos - 0.08) ** 2) / 0.01) * (0.6 + 0.4 * math.sin(t * 1.7))
            mid  = 0.5  * math.exp(-((pos - 0.35) ** 2) / 0.02) * (0.5 + 0.5 * math.sin(t * 2.3 + 1))
            high = 0.25 * math.exp(-((pos - 0.75) ** 2) / 0.04) * (0.4 + 0.6 * math.sin(t * 3.1 + 2))
            bars[i] = min(1.0, bass + mid + high + np.random.uniform(0, 0.06))
        attack        = np.where(bars > self.fft_bars, 0.6, 0.0)
        decay         = np.where(bars <= self.fft_bars, 0.07, 0.0)
        self.fft_bars = np.clip(self.fft_bars * (1 - attack - decay) + bars * (attack + decay), 0, 1)

    def get_bars(self):
        return self.fft_bars.copy()

    def get_rms(self):
        return float(np.mean(self.fft_bars))


FAKE_SONG = {
    'title':  'Blue in Green',
    'artist': 'Miles Davis',
    'art':    None,
    'bg':     None,
    'status': 'Identified',
}
