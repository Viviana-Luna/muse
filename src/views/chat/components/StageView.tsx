import { useEffect, useState } from 'react';
import type { RefObject } from 'react';

interface StageViewProps {
  stageStateClass: string;
  portraitPath: string;
  personaName?: string;
  showPortrait?: boolean;
  visualizerCanvasRef?: RefObject<HTMLCanvasElement | null>;
}

export function StageView({
  stageStateClass,
  portraitPath,
  personaName,
  showPortrait = true,
  visualizerCanvasRef
}: StageViewProps) {
  const [portraitFailed, setPortraitFailed] = useState(false);

  useEffect(() => setPortraitFailed(false), [portraitPath]);

  return (
    <section className={`stage ${stageStateClass}`} aria-label="角色舞台">
      <div className="stage-vignette" />
      {showPortrait && portraitPath && !portraitFailed && (
        <div className="portrait-wrap">
          <div className="portrait-frame">
            <img
              className="portrait"
              src={portraitPath}
              alt={personaName || '角色立绘'}
              draggable={false}
              onError={() => setPortraitFailed(true)}
            />
          </div>
        </div>
      )}
    </section>
  );
}
