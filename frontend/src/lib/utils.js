export function formatTime(ms) {
  if (ms == null || ms < 0) return '0:00';
  const totalSeconds = Math.floor(ms / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${seconds.toString().padStart(2, '0')}`;
}

export function formatTimeFull(ms) {
  if (ms == null || ms < 0) return '0:00:00';
  const totalSeconds = Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) {
    return `${hours}:${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}`;
  }
  return `${minutes}:${seconds.toString().padStart(2, '0')}`;
}

/**
 * Native (comprehension-language) subtitle lines that temporally overlap the
 * range [startMs, endMs]. Used to reveal the native text under a target line.
 * @param {{start_ms:number,end_ms:number,text:string}[]} nativeLines
 * @param {number} startMs
 * @param {number} endMs
 */
export function nativeLinesForRange(nativeLines, startMs, endMs) {
  if (!nativeLines || !nativeLines.length || startMs == null || endMs == null) return [];
  return nativeLines.filter((line) => line.start_ms <= endMs && line.end_ms >= startMs);
}

/**
 * Gather a native-language translation string for a mined range. A native line
 * is included when it is either fully contained in [startMs, endMs] or shares
 * similar bounds within `tolMs` on both ends. Returns the joined text.
 * @param {{start_ms:number,end_ms:number,text:string}[]} nativeLines
 * @param {number} startMs
 * @param {number} endMs
 * @param {number} [tolMs]
 */
export function gatherTranslation(nativeLines, startMs, endMs, tolMs = 500) {
  if (!nativeLines || !nativeLines.length || startMs == null || endMs == null) return '';
  return nativeLines
    .filter((line) => {
      const contained = line.start_ms >= startMs && line.end_ms <= endMs;
      const similarBounds =
        Math.abs(line.start_ms - startMs) <= tolMs && Math.abs(line.end_ms - endMs) <= tolMs;
      return contained || similarBounds;
    })
    .map((line) => line.text)
    .join('\n')
    .trim();
}

export function audioMimeType(format, mimeType = null) {
  if (mimeType) return mimeType;
  switch (format) {
    case 'mp3':
      return 'audio/mpeg';
    case 'aac':
      return 'audio/mp4';
    case 'opus':
      return 'audio/ogg; codecs=opus';
    default:
      return 'application/octet-stream';
  }
}

export function imageMimeType(format, mimeType = null) {
  if (mimeType) return mimeType;
  switch (format) {
    case 'jpeg':
    case 'jpg':
      return 'image/jpeg';
    case 'webp':
      return 'image/webp';
    case 'png':
      return 'image/png';
    case 'avif':
      return 'image/avif';
    default:
      return 'application/octet-stream';
  }
}
