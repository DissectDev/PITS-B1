import React, { useEffect, useRef, useState } from 'react';
import { useCreateBlockNote } from '@blocknote/react';
import { BlockNoteView } from '@blocknote/shadcn';
import '@blocknote/shadcn/style.css';
import { cn } from '../../lib/utils';

export interface BlockNoteMarkdownEditorProps {
  /** Key identifying the loaded document (e.g. taskKey or projectId). Reloads blocks when changed. */
  docKey: string;
  initialMarkdown: string;
  onMarkdownChange: (markdown: string) => void;
  editable?: boolean;
  className?: string;
}

function readDomDarkMode(): boolean {
  if (typeof document === 'undefined') return false;
  return (
    document.documentElement.classList.contains('dark') ||
    document.documentElement.getAttribute('data-theme-mode') === 'dark'
  );
}

export const BlockNoteMarkdownEditor: React.FC<BlockNoteMarkdownEditorProps> = ({
  docKey,
  initialMarkdown,
  onMarkdownChange,
  editable = true,
  className,
}) => {
  const editor = useCreateBlockNote();
  const [isDark, setIsDark] = useState<boolean>(readDomDarkMode);
  const hydratingRef = useRef<boolean>(false);
  const loadedKeyRef = useRef<string>('');
  const lastEmittedRef = useRef<string>(initialMarkdown);

  useEffect(() => {
    if (typeof document === 'undefined') return;
    const obs = new MutationObserver(() => {
      setIsDark(readDomDarkMode());
    });
    obs.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['class', 'data-theme-mode'],
    });
    return () => obs.disconnect();
  }, []);

  useEffect(() => {
    if (loadedKeyRef.current === docKey && initialMarkdown === lastEmittedRef.current) {
      return;
    }
    let cancelled = false;
    (async () => {
      hydratingRef.current = true;
      try {
        const blocks = await editor.tryParseMarkdownToBlocks(initialMarkdown || '');
        if (!cancelled) {
          editor.replaceBlocks(editor.document, blocks);
          loadedKeyRef.current = docKey;
          lastEmittedRef.current = initialMarkdown || '';
        }
      } catch {
        // Fallback: keep current editor document if Markdown parser encounters unusual syntax
      } finally {
        if (!cancelled) {
          hydratingRef.current = false;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [docKey, initialMarkdown, editor]);

  const handleChange = async () => {
    if (hydratingRef.current) return;
    try {
      const md = await editor.blocksToMarkdownLossy(editor.document);
      lastEmittedRef.current = md;
      onMarkdownChange(md);
    } catch {
      // Ignore transient conversion failures during rapid typing
    }
  };

  return (
    <div
      className={cn(
        'pits-shadcn-root overflow-hidden rounded-lg border border-solid border-border bg-card',
        className,
      )}
    >
      <BlockNoteView
        editor={editor}
        editable={editable}
        theme={isDark ? 'dark' : 'light'}
        onChange={handleChange}
      />
    </div>
  );
};
