import { useEffect, useMemo, useRef, useState } from 'react';
import type { CSSProperties, KeyboardEvent, PointerEvent, WheelEvent } from 'react';
import { createPortal } from 'react-dom';

import { useModalAccessibility } from '@/hooks/useModalAccessibility';
import {
  clampPersonaCropSelection,
  createInitialPersonaCropSelection,
  createPersonaImageCrops,
  panPersonaCropSelection,
  PERSONA_CROP_TARGETS,
  PERSONA_CROP_ZOOM_MAX,
  PERSONA_CROP_ZOOM_MIN
} from '@/views/personas/utils/personaImageCrop';
import type {
  PersonaCropSelection,
  PersonaCropTargetKey
} from '@/views/personas/utils/personaImageCrop';

interface PersonaImageCropDialogProps {
  file: File;
  busy: boolean;
  onCancel: () => void;
  onConfirm: (files: Record<PersonaCropTargetKey, File>) => Promise<boolean>;
}

interface ImageSize {
  width: number;
  height: number;
}

const EMPTY_SELECTION: PersonaCropSelection = { centerX: 0, centerY: 0, zoom: 1 };

export function PersonaImageCropDialog({
  file,
  busy,
  onCancel,
  onConfirm
}: PersonaImageCropDialogProps) {
  const [activeTarget, setActiveTarget] = useState<PersonaCropTargetKey>('portrait');
  const [imageSize, setImageSize] = useState<ImageSize | null>(null);
  const [viewportSize, setViewportSize] = useState<ImageSize>({ width: 0, height: 0 });
  const [selections, setSelections] = useState<Record<PersonaCropTargetKey, PersonaCropSelection>>({
    portrait: EMPTY_SELECTION,
    avatar: EMPTY_SELECTION
  });
  const [processing, setProcessing] = useState(false);
  const [error, setError] = useState('');
  const frameRef = useRef<HTMLDivElement | null>(null);
  const dragRef = useRef<{ pointerId: number; x: number; y: number } | null>(null);
  const dialogRef = useModalAccessibility<HTMLElement>(true, () => {
    if (!busy && !processing) onCancel();
  });
  const sourceUrl = useMemo(() => URL.createObjectURL(file), [file]);
  const target = PERSONA_CROP_TARGETS[activeTarget];
  const selection = selections[activeTarget];
  const locked = busy || processing;

  useEffect(() => () => URL.revokeObjectURL(sourceUrl), [sourceUrl]);

  useEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;
    const measure = () => {
      const rect = frame.getBoundingClientRect();
      setViewportSize({ width: rect.width, height: rect.height });
    };
    measure();
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measure);
    observer.observe(frame);
    return () => observer.disconnect();
  }, [activeTarget]);

  function updateSelection(
    key: PersonaCropTargetKey,
    updater: (current: PersonaCropSelection) => PersonaCropSelection
  ) {
    setSelections((current) => ({ ...current, [key]: updater(current[key]) }));
  }

  function initializeImage(width: number, height: number) {
    if (width <= 0 || height <= 0) {
      setError('图片尺寸无效，请重新选择原图。');
      return;
    }
    setImageSize({ width, height });
    const initial = createInitialPersonaCropSelection(width, height);
    setSelections({ portrait: initial, avatar: initial });
    setError('');
  }

  function changeZoom(nextZoom: number) {
    if (!imageSize) return;
    updateSelection(activeTarget, (current) =>
      clampPersonaCropSelection(imageSize.width, imageSize.height, target, {
        ...current,
        zoom: nextZoom
      })
    );
  }

  function panImage(deltaX: number, deltaY: number) {
    if (!imageSize) return;
    updateSelection(activeTarget, (current) =>
      panPersonaCropSelection(
        imageSize.width,
        imageSize.height,
        target,
        current,
        deltaX,
        deltaY,
        viewportSize.width,
        viewportSize.height
      )
    );
  }

  function handlePointerDown(event: PointerEvent<HTMLDivElement>) {
    if (locked || !imageSize) return;
    dragRef.current = { pointerId: event.pointerId, x: event.clientX, y: event.clientY };
    event.currentTarget.setPointerCapture(event.pointerId);
  }

  function handlePointerMove(event: PointerEvent<HTMLDivElement>) {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    panImage(event.clientX - drag.x, event.clientY - drag.y);
    dragRef.current = { pointerId: event.pointerId, x: event.clientX, y: event.clientY };
  }

  function handlePointerEnd(event: PointerEvent<HTMLDivElement>) {
    if (dragRef.current?.pointerId !== event.pointerId) return;
    dragRef.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
  }

  function handleWheel(event: WheelEvent<HTMLDivElement>) {
    if (locked || !imageSize) return;
    event.preventDefault();
    changeZoom(selection.zoom + (event.deltaY < 0 ? 0.08 : -0.08));
  }

  function handleCropKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const step = event.shiftKey ? 24 : 8;
    const delta =
      event.key === 'ArrowLeft'
        ? [step, 0]
        : event.key === 'ArrowRight'
          ? [-step, 0]
          : event.key === 'ArrowUp'
            ? [0, step]
            : event.key === 'ArrowDown'
              ? [0, -step]
              : null;
    if (!delta) return;
    event.preventDefault();
    panImage(delta[0], delta[1]);
  }

  async function confirmCrop() {
    if (!imageSize || locked) return;
    setProcessing(true);
    setError('');
    let accepted = false;
    try {
      const files = await createPersonaImageCrops(file, selections);
      accepted = await onConfirm(files);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : '裁剪图片时遇到错误。');
    } finally {
      setProcessing(false);
    }
    if (accepted) onCancel();
  }

  const imageStyle = (() => {
    if (!imageSize || viewportSize.width <= 0 || viewportSize.height <= 0) return undefined;
    const coverScale = Math.max(
      viewportSize.width / imageSize.width,
      viewportSize.height / imageSize.height
    );
    const scale = coverScale * selection.zoom;
    return {
      width: `${imageSize.width * scale}px`,
      height: `${imageSize.height * scale}px`,
      left: `${viewportSize.width / 2 - selection.centerX * scale}px`,
      top: `${viewportSize.height / 2 - selection.centerY * scale}px`
    } as CSSProperties;
  })();

  const dialog = (
    <section
      className="modal-shell stacked persona-crop-modal"
      ref={dialogRef}
      tabIndex={-1}
      role="dialog"
      aria-modal="true"
      aria-label="裁剪角色图片"
    >
      <div className="persona-crop-dialog">
        <header>
          <div>
            <strong>裁剪角色图片</strong>
            <span>同一张原图分别生成固定尺寸的立绘与头像。</span>
          </div>
          <button type="button" disabled={locked} onClick={onCancel}>关闭</button>
        </header>

        <div className="persona-crop-body">
          <div className="persona-crop-tabs" role="tablist" aria-label="裁剪目标">
            {(Object.keys(PERSONA_CROP_TARGETS) as PersonaCropTargetKey[]).map((key) => {
              const item = PERSONA_CROP_TARGETS[key];
              return (
                <button
                  type="button"
                  role="tab"
                  aria-selected={activeTarget === key}
                  className={activeTarget === key ? 'active' : ''}
                  key={key}
                  onClick={() => setActiveTarget(key)}
                >
                  <strong>{item.label}</strong>
                  <small>{item.width} × {item.height}</small>
                </button>
              );
            })}
          </div>

          <div className="persona-crop-stage">
            <div
              className={`persona-crop-frame is-${activeTarget}`}
              ref={frameRef}
              tabIndex={0}
              role="application"
              aria-label={`${target.label}裁剪区域，可拖动图片，使用方向键移动`}
              onPointerDown={handlePointerDown}
              onPointerMove={handlePointerMove}
              onPointerUp={handlePointerEnd}
              onPointerCancel={handlePointerEnd}
              onWheel={handleWheel}
              onKeyDown={handleCropKeyDown}
            >
              <img
                src={sourceUrl}
                alt=""
                draggable={false}
                style={imageStyle}
                onLoad={(event) =>
                  initializeImage(event.currentTarget.naturalWidth, event.currentTarget.naturalHeight)
                }
                onError={() => setError('图片无法读取，请重新选择 PNG、JPEG 或 WebP 原图。')}
              />
              <span className="persona-crop-guide" aria-hidden="true" />
            </div>
            <p>拖动图片调整位置，滚轮或下方滑杆缩放；方向键可精细移动。</p>
          </div>

          <div className="persona-crop-controls">
            <label htmlFor="persona-crop-zoom">
              缩放
              <input
                id="persona-crop-zoom"
                type="range"
                min={PERSONA_CROP_ZOOM_MIN}
                max={PERSONA_CROP_ZOOM_MAX}
                step="0.01"
                value={selection.zoom}
                disabled={locked || !imageSize}
                onChange={(event) => changeZoom(Number(event.target.value))}
              />
              <output>{Math.round(selection.zoom * 100)}%</output>
            </label>
            <button
              type="button"
              disabled={locked || !imageSize}
              onClick={() => {
                if (!imageSize) return;
                updateSelection(
                  activeTarget,
                  () => createInitialPersonaCropSelection(imageSize.width, imageSize.height)
                );
              }}
            >
              重置当前裁剪
            </button>
          </div>

          {error && <p className="persona-crop-error" role="alert">{error}</p>}
        </div>

        <footer>
          <span>原图不会保存；再次调整需要重新选择原图。</span>
          <button type="button" disabled={locked} onClick={onCancel}>取消</button>
          <button
            type="button"
            disabled={locked || !imageSize || Boolean(error)}
            onClick={() => void confirmCrop()}
          >
            {locked ? '正在生成并上传' : '确认并使用'}
          </button>
        </footer>
      </div>
    </section>
  );

  const portalHost = document.querySelector('.runtime-shell') ?? document.body;
  return createPortal(dialog, portalHost);
}
