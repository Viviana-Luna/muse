import { useEffect, useRef, useState } from 'react';
import { ImageOff } from 'lucide-react';

import { useAuthenticatedAssetUrl } from '@/hooks/useAuthenticatedAsset';
import type { PersonaVisualPreview } from '@/types';

function useNearViewport() {
  const elementRef = useRef<HTMLDivElement | null>(null);
  const [visible, setVisible] = useState(() => typeof IntersectionObserver === 'undefined');

  useEffect(() => {
    if (visible) return;
    const element = elementRef.current;
    if (!element) return;
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (!entry?.isIntersecting) return;
        setVisible(true);
        observer.disconnect();
      },
      { rootMargin: '240px' }
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [visible]);

  return { elementRef, visible };
}

interface PersonaCardMediaProps {
  name: string;
  preview: PersonaVisualPreview;
}

/** 角色画廊图片：按需解析受保护资源；缺失或损坏时明确展示无图状态。 */
export function PersonaCardMedia({ name, preview }: PersonaCardMediaProps) {
  const sourcePath = preview.avatar_path?.trim() || preview.portrait_path?.trim() || '';
  const { elementRef, visible } = useNearViewport();
  const resolvedUrl = useAuthenticatedAssetUrl(visible ? sourcePath : '');
  const [failed, setFailed] = useState(false);

  useEffect(() => setFailed(false), [sourcePath]);

  const waitingForAsset = Boolean(sourcePath && !failed && (!visible || !resolvedUrl));
  const hasImage = Boolean(sourcePath && !failed && resolvedUrl);
  const isEmpty = !sourcePath || failed;

  return (
    <div
      ref={elementRef}
      className={`persona-card-media${waitingForAsset ? ' loading' : ''}`}
      data-empty={isEmpty ? 'true' : 'false'}
    >
      {hasImage && (
        <img
          src={resolvedUrl}
          alt=""
          aria-hidden="true"
          loading="lazy"
          draggable={false}
          onError={() => setFailed(true)}
        />
      )}
      {isEmpty && (
        <div className="persona-card-media-empty" role="img" aria-label={`角色“${name}”暂无图片`}>
          <ImageOff aria-hidden="true" />
          <span>暂无图片</span>
        </div>
      )}
    </div>
  );
}
