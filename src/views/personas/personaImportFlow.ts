interface BeginPersonaImportOptions {
  markPending: () => void;
  openLibrary: () => void;
}

/** 记录导入意图并打开角色库；角色库挂载后会进入可审阅的导入流程。 */
export function beginPersonaImport({
  markPending,
  openLibrary
}: BeginPersonaImportOptions) {
  markPending();
  openLibrary();
}
