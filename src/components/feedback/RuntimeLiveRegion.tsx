interface RuntimeLiveRegionProps {
  status?: string;
}

function shortStatus(status?: string): string {
  const normalized = status?.replace(/\s+/gu, ' ').trim() ?? '';
  if (!normalized) return '';
  const sentence = normalized.split(/[。！？\n]/u)[0];
  return sentence.slice(0, 80);
}

export function RuntimeLiveRegion({ status }: RuntimeLiveRegionProps) {
  return (
    <p className="sr-only" role="status" aria-live="polite" aria-atomic="true">
      {shortStatus(status)}
    </p>
  );
}
