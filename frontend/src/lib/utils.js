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
