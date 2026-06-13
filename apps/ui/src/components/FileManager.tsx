import React, { useState, useEffect, useMemo } from 'react';
import { Folder, File, Download, Trash2, Edit2, ArrowUp, Search, Plus, Loader2, Home, Upload, ChevronRight, X, AlertTriangle } from 'lucide-react';
import { invoke, listen } from '../store/ipc';
import { useToastStore } from '../store/toastStore';

interface FileEntry {
  name: string;
  is_dir: boolean;
  size: number;
  modified: number;
}

export function FileManager() {
  const [currentPath, setCurrentPath] = useState<string>('');
  const currentPathRef = React.useRef(currentPath);
  currentPathRef.current = currentPath;
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  
  // UI filter / search
  const [searchQuery, setSearchQuery] = useState<string>('');
  
  // Folder creation & renaming dialogs
  const [showCreateFolder, setShowCreateFolder] = useState<boolean>(false);
  const [newFolderName, setNewFolderName] = useState<string>('');
  const [renamingEntry, setRenamingEntry] = useState<FileEntry | null>(null);
  const [newName, setNewName] = useState<string>('');
  
  // Deletion confirmation
  const [deletingEntry, setDeletingEntry] = useState<FileEntry | null>(null);
  const [actionLoading, setActionLoading] = useState<boolean>(false);

  // Active transfers progress tracking
  const [transferStatus, setTransferStatus] = useState<{
    type: 'upload' | 'download' | null;
    name: string;
    progress: number; // 0 to 100
  }>({ type: null, name: '', progress: 0 });

  const addToast = useToastStore(s => s.addToast);

  // Fetch directory contents
  const loadDirectory = async (path: string) => {
    setLoading(true);
    setError(null);
    try {
      await invoke('list_host_dir', { path });
    } catch (err: any) {
      console.error(err);
      setError(err.message || 'Failed to request folder listings');
      setLoading(false);
    }
  };

  useEffect(() => {
    // Initial fetch (empty string lets backend decide default folder)
    loadDirectory('');

    // Listen to directory responses
    let unlistenDir: any = null;
    let unlistenAction: any = null;
    let unlistenDownStart: any = null;
    let unlistenDownChunk: any = null;
    let unlistenDownEnd: any = null;

    listen<any>('host_directory_list', (ev) => {
      const { path, entries: list, error: err } = ev.payload;
      setLoading(false);
      if (err) {
        setError(err);
      } else {
        setCurrentPath(path);
        setEntries(list || []);
      }
    }).then(un => unlistenDir = un);

    // Listen to delete/rename action finished responses
    listen<any>('file_action_finished', (ev) => {
      setActionLoading(false);
      const { success, error: err } = ev.payload;
      if (success) {
        addToast('Action Completed', 'File system operation completed successfully.', 'success');
        loadDirectory(currentPathRef.current);
      } else {
        addToast('Operation Failed', err || 'File system operation failed.', 'error');
      }
    }).then(un => unlistenAction = un);

    // Listen to file download progress from backend
    listen<any>('file_download_start', (ev) => {
      const { name, size } = ev.payload;
      setTransferStatus({ type: 'download', name, progress: 0 });
    }).then(un => unlistenDownStart = un);

    listen<any>('file_download_chunk', (ev) => {
      // Chunk arrived, we don't have direct index here, but we can animate progress increment or completion
      setTransferStatus(prev => {
        if (prev.type === 'download') {
          const nextProg = Math.min(prev.progress + 4, 98);
          return { ...prev, progress: nextProg };
        }
        return prev;
      });
    }).then(un => unlistenDownChunk = un);

    listen<any>('file_download_end', () => {
      setTransferStatus(prev => {
        addToast('Download Completed', `Successfully downloaded file ${prev.name} to your local Downloads folder.`, 'success');
        return { type: null, name: '', progress: 0 };
      });
      loadDirectory(currentPathRef.current);
    }).then(un => unlistenDownEnd = un);

    return () => {
      if (unlistenDir) unlistenDir();
      if (unlistenAction) unlistenAction();
      if (unlistenDownStart) unlistenDownStart();
      if (unlistenDownChunk) unlistenDownChunk();
      if (unlistenDownEnd) unlistenDownEnd();
    };
  }, [addToast]);

  // Navigate deeper into directory
  const handleNavigate = (entry: FileEntry) => {
    if (!entry.is_dir) return;
    const separator = currentPath.includes('\\') ? '\\' : '/';
    const newPath = currentPath.endsWith(separator) || currentPath === '' 
      ? `${currentPath}${entry.name}` 
      : `${currentPath}${separator}${entry.name}`;
    loadDirectory(newPath);
  };

  // Navigate to parent directory
  const handleNavigateUp = () => {
    const isWindows = currentPath.includes('\\') || /^[a-zA-Z]:/.test(currentPath);
    const separator = isWindows ? '\\' : '/';
    
    // Split and filter out empty segments
    const segments = currentPath.split(separator).filter(Boolean);
    if (segments.length <= 1) {
      // If we are at root level (e.g. C:\ or /), navigate to empty string to list default/drives
      loadDirectory('');
      return;
    }
    
    segments.pop();
    let parentPath = segments.join(separator);
    // Restore Windows drive layout
    if (isWindows && /^[a-zA-Z]$/.test(parentPath)) {
      parentPath = parentPath + ':';
    }
    if (!isWindows && !parentPath.startsWith('/')) {
      parentPath = '/' + parentPath;
    }
    loadDirectory(parentPath);
  };

  // Format file size
  const formatSize = (bytes: number) => {
    if (bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
  };

  // Format Unix epoch timestamp
  const formatDate = (epochMs: number) => {
    if (epochMs === 0) return '—';
    return new Date(epochMs).toLocaleDateString(undefined, {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit'
    });
  };

  // Trigger downloads
  const handleDownload = async (entry: FileEntry) => {
    const separator = currentPath.includes('\\') ? '\\' : '/';
    const fullPath = currentPath.endsWith(separator) ? `${currentPath}${entry.name}` : `${currentPath}${separator}${entry.name}`;
    
    addToast('Download Starting', `Requesting download of ${entry.name} from host...`, 'info');
    try {
      await invoke('download_host_file', { path: fullPath });
    } catch (err: any) {
      addToast('Download Failed', err.message || 'Failed to start download', 'error');
    }
  };

  // Trigger file action requests (delete, rename, create directory)
  const handleAction = async (action: 'delete' | 'rename' | 'create_dir', targetPath: string, renameTo?: string) => {
    setActionLoading(true);
    try {
      await invoke('host_file_action', {
        action,
        path: targetPath,
        newPath: renameTo
      });
    } catch (err: any) {
      setActionLoading(false);
      addToast('Operation Failed', err.message || `Failed to perform file action ${action}`, 'error');
    }
  };

  // Perform create directory action
  const handleCreateFolderSubmit = () => {
    if (!newFolderName.trim()) return;
    const separator = currentPath.includes('\\') ? '\\' : '/';
    const fullPath = currentPath.endsWith(separator) ? `${currentPath}${newFolderName}` : `${currentPath}${separator}${newFolderName}`;
    
    setShowCreateFolder(false);
    setNewFolderName('');
    handleAction('create_dir', fullPath);
  };

  // Perform rename action
  const handleRenameSubmit = () => {
    if (!renamingEntry || !newName.trim()) return;
    const separator = currentPath.includes('\\') ? '\\' : '/';
    const fullPath = currentPath.endsWith(separator) ? `${currentPath}${renamingEntry.name}` : `${currentPath}${separator}${renamingEntry.name}`;
    const newFullPath = currentPath.endsWith(separator) ? `${currentPath}${newName}` : `${currentPath}${separator}${newName}`;
    
    setRenamingEntry(null);
    setNewName('');
    handleAction('rename', fullPath, newFullPath);
  };

  // Perform delete action
  const handleDeleteConfirm = () => {
    if (!deletingEntry) return;
    const separator = currentPath.includes('\\') ? '\\' : '/';
    const fullPath = currentPath.endsWith(separator) ? `${currentPath}${deletingEntry.name}` : `${currentPath}${separator}${deletingEntry.name}`;
    
    setDeletingEntry(null);
    handleAction('delete', fullPath);
  };

  // Perform file upload (read file as chunk blobs and stream base64 chunks)
  const handleUploadFile = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;

    setTransferStatus({ type: 'upload', name: file.name, progress: 0 });
    addToast('Upload Starting', `Uploading ${file.name} to host directory...`, 'info');

    try {
      const separator = currentPath.includes('\\') ? '\\' : '/';
      const fullPath = currentPath.endsWith(separator) ? `${currentPath}${file.name}` : `${currentPath}${separator}${file.name}`;
      
      await invoke('send_file_start', { name: fullPath, size: file.size });

      const chunkSize = 64 * 1024; // 64KB chunks
      let offset = 0;
      
      const readNextChunk = () => {
        const slice = file.slice(offset, offset + chunkSize);
        const reader = new FileReader();
        
        reader.onload = async (event) => {
          if (event.target?.result) {
            const arrayBuffer = event.target.result as ArrayBuffer;
            
            // Base64 encode chunk
            let binary = '';
            const bytes = new Uint8Array(arrayBuffer);
            const len = bytes.byteLength;
            for (let i = 0; i < len; i++) {
              binary += String.fromCharCode(bytes[i]);
            }
            const b64Data = window.btoa(binary);

            try {
              await invoke('send_file_chunk', { data: b64Data });
              offset += len;
              const progress = Math.round((offset / file.size) * 100);
              setTransferStatus({ type: 'upload', name: file.name, progress });

              if (offset < file.size) {
                readNextChunk();
              } else {
                await invoke('send_file_end');
                setTransferStatus({ type: null, name: '', progress: 0 });
                addToast('Upload Completed', `Successfully uploaded ${file.name}.`, 'success');
                loadDirectory(currentPath);
              }
            } catch (err: any) {
              setTransferStatus({ type: null, name: '', progress: 0 });
              addToast('Upload Failed', `Error sending chunk: ${err.message}`, 'error');
            }
          }
        };

        reader.onerror = () => {
          setTransferStatus({ type: null, name: '', progress: 0 });
          addToast('Upload Failed', 'Failed to read local file contents.', 'error');
        };

        reader.readAsArrayBuffer(slice);
      };

      readNextChunk();
    } catch (err: any) {
      setTransferStatus({ type: null, name: '', progress: 0 });
      addToast('Upload Failed', err.message || 'Failed to initiate upload', 'error');
    }
  };

  // Filter list by query
  const filteredEntries = useMemo(() => {
    return entries.filter(e => e.name.toLowerCase().includes(searchQuery.toLowerCase()));
  }, [entries, searchQuery]);

  // Build path breadcrumbs
  const breadcrumbs = useMemo(() => {
    const isWindows = currentPath.includes('\\') || /^[a-zA-Z]:/.test(currentPath);
    const separator = isWindows ? '\\' : '/';
    const segments = currentPath.split(separator).filter(Boolean);
    
    return segments.map((seg, idx) => {
      const segmentPath = segments.slice(0, idx + 1).join(separator);
      return {
        label: seg,
        path: isWindows ? segmentPath : '/' + segmentPath
      };
    });
  }, [currentPath]);

  return (
    <div className="glass-panel" style={{ display: 'flex', flexDirection: 'column', height: '100%', padding: '16px', gap: '14px' }}>
      
      {/* File Manager Toolbar */}
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', flexWrap: 'wrap', gap: '10px' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
          <button 
            className="btn btn-ghost btn-sm" 
            onClick={handleNavigateUp} 
            disabled={currentPath === '' || loading}
            title="Up one level"
          >
            <ArrowUp size={16} />
          </button>
          <button 
            className="btn btn-ghost btn-sm" 
            onClick={() => loadDirectory('')}
            disabled={loading}
            title="Home Directory"
          >
            <Home size={16} />
          </button>
          
          {/* Breadcrumbs */}
          <div style={{ display: 'flex', alignItems: 'center', gap: '4px', fontSize: '0.85rem', color: 'var(--text-secondary)' }}>
            <span style={{ cursor: 'pointer' }} onClick={() => loadDirectory('')}>Root</span>
            {breadcrumbs.map((crumb, idx) => (
              <React.Fragment key={idx}>
                <ChevronRight size={12} />
                <span 
                  style={{ 
                    cursor: 'pointer', 
                    color: idx === breadcrumbs.length - 1 ? 'var(--text-primary)' : 'var(--text-secondary)',
                    fontWeight: idx === breadcrumbs.length - 1 ? 600 : 400
                  }}
                  onClick={() => loadDirectory(crumb.path)}
                >
                  {crumb.label}
                </span>
              </React.Fragment>
            ))}
          </div>
        </div>

        <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
          {/* Search bar */}
          <div style={{ position: 'relative', display: 'flex', alignItems: 'center' }}>
            <Search size={14} style={{ position: 'absolute', left: '10px', color: 'var(--text-secondary)' }} />
            <input
              type="text"
              placeholder="Search files..."
              value={searchQuery}
              onChange={e => setSearchQuery(e.target.value)}
              style={{ paddingLeft: '32px', fontSize: '0.8rem', height: '32px', width: '180px' }}
            />
          </div>

          {/* New folder */}
          <button 
            className="btn btn-ghost btn-sm" 
            onClick={() => setShowCreateFolder(true)}
            disabled={loading}
          >
            <Plus size={14} /> New Folder
          </button>

          {/* Upload file */}
          <label className="btn btn-primary btn-sm" style={{ cursor: 'pointer', display: 'flex', alignItems: 'center', gap: '6px' }}>
            <Upload size={14} /> Upload
            <input 
              type="file" 
              onChange={handleUploadFile} 
              style={{ display: 'none' }}
              disabled={loading} 
            />
          </label>
        </div>
      </div>

      {/* Path Address Bar */}
      <div style={{ display: 'flex', gap: '8px' }}>
        <input 
          type="text" 
          value={currentPath} 
          onChange={e => setCurrentPath(e.target.value)}
          placeholder="Path address (e.g. C:\Users or /home/user)"
          onKeyDown={e => e.key === 'Enter' && loadDirectory(currentPath)}
          style={{ flex: 1, fontSize: '0.8rem', height: '34px', fontFamily: 'monospace' }}
        />
        <button className="btn btn-ghost btn-sm" onClick={() => loadDirectory(currentPath)} disabled={loading}>
          Go
        </button>
      </div>

      {/* Transfer Progress Panel overlay */}
      {transferStatus.type && (
        <div style={{
          padding: '12px 16px',
          background: 'rgba(59, 130, 246, 0.08)',
          border: '1px solid rgba(59, 130, 246, 0.3)',
          borderRadius: 'var(--radius-md)',
          display: 'flex',
          flexDirection: 'column',
          gap: '8px'
        }}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', fontSize: '0.8rem' }}>
            <span style={{ fontWeight: 600, display: 'flex', alignItems: 'center', gap: '8px' }}>
              <Loader2 size={14} className="spinner" /> 
              {transferStatus.type === 'upload' ? 'Uploading' : 'Downloading'}: {transferStatus.name}
            </span>
            <span style={{ color: 'var(--text-secondary)' }}>{transferStatus.progress}%</span>
          </div>
          <div style={{ background: 'rgba(255,255,255,0.05)', height: '6px', borderRadius: '3px', overflow: 'hidden' }}>
            <div style={{ background: 'var(--primary)', width: `${transferStatus.progress}%`, height: '100%', transition: 'width 0.2s ease' }} />
          </div>
        </div>
      )}

      {/* Explorer Content list */}
      <div style={{ flex: 1, overflowY: 'auto', maxHeight: 'calc(100vh - 360px)', position: 'relative' }}>
        {loading ? (
          <div className="empty-state">
            <Loader2 className="spinner" size={24} />
            <p>Reading remote file list...</p>
          </div>
        ) : error ? (
          <div className="empty-state" style={{ color: 'var(--danger)' }}>
            <span style={{ fontSize: '24px' }}>⚠️</span>
            <p style={{ marginTop: '8px', fontWeight: 500 }}>{error}</p>
            <button className="btn btn-ghost btn-sm" onClick={() => loadDirectory(currentPath)} style={{ marginTop: '12px' }}>
              Retry Fetch
            </button>
          </div>
        ) : filteredEntries.length === 0 ? (
          <div className="empty-state">
            <span style={{ fontSize: '24px' }}>📂</span>
            <p>This folder is empty or no results matched.</p>
          </div>
        ) : (
          <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.8rem' }}>
            <thead>
              <tr style={{ borderBottom: '1px solid var(--border)', color: 'var(--text-secondary)', textAlign: 'left' }}>
                <th style={{ padding: '8px' }}>Name</th>
                <th style={{ padding: '8px', width: '100px' }}>Size</th>
                <th style={{ padding: '8px', width: '150px' }}>Date Modified</th>
                <th style={{ padding: '8px', width: '120px', textAlign: 'right' }}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {filteredEntries.map((entry, idx) => (
                <tr 
                  key={idx} 
                  style={{ 
                    borderBottom: '1px solid rgba(255,255,255,0.02)',
                    cursor: entry.is_dir ? 'pointer' : 'default',
                  }}
                  className="table-row-hover"
                >
                  <td 
                    style={{ padding: '10px 8px', display: 'flex', alignItems: 'center', gap: '8px', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
                    onClick={() => entry.is_dir && handleNavigate(entry)}
                  >
                    {entry.is_dir ? (
                      <Folder size={16} style={{ color: '#3b82f6', flexShrink: 0 }} />
                    ) : (
                      <File size={16} style={{ color: 'var(--text-secondary)', flexShrink: 0 }} />
                    )}
                    <span style={{ fontWeight: entry.is_dir ? 500 : 400 }}>{entry.name}</span>
                  </td>
                  <td style={{ padding: '10px 8px', color: 'var(--text-secondary)' }}>
                    {entry.is_dir ? 'Folder' : formatSize(entry.size)}
                  </td>
                  <td style={{ padding: '10px 8px', color: 'var(--text-secondary)' }}>
                    {formatDate(entry.modified)}
                  </td>
                  <td style={{ padding: '10px 8px', textAlign: 'right' }}>
                    <div style={{ display: 'flex', gap: '4px', justifyContent: 'flex-end' }}>
                      {!entry.is_dir && (
                        <button 
                          className="btn btn-ghost btn-xs" 
                          onClick={() => handleDownload(entry)}
                          title="Download file"
                          disabled={actionLoading}
                        >
                          <Download size={12} />
                        </button>
                      )}
                      <button 
                        className="btn btn-ghost btn-xs" 
                        onClick={() => {
                          setRenamingEntry(entry);
                          setNewName(entry.name);
                        }}
                        title="Rename"
                        disabled={actionLoading}
                      >
                        <Edit2 size={12} />
                      </button>
                      <button 
                        className="btn btn-ghost btn-xs text-danger" 
                        onClick={() => setDeletingEntry(entry)}
                        title="Delete"
                        disabled={actionLoading}
                      >
                        <Trash2 size={12} />
                      </button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {/* Folder Creation Modal */}
      {showCreateFolder && (
        <div className="glass-panel" style={{
          position: 'fixed', top: '50%', left: '50%', transform: 'translate(-50%, -50%)',
          zIndex: 1000, display: 'flex', flexDirection: 'column', gap: '16px',
          width: '320px', padding: '20px', background: 'rgba(12, 10, 20, 0.95)', border: '1px solid var(--border)'
        }}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
            <span style={{ fontWeight: 600, fontSize: '0.9rem' }}>Create New Folder</span>
            <X size={16} style={{ cursor: 'pointer' }} onClick={() => setShowCreateFolder(false)} />
          </div>
          <input
            type="text"
            placeholder="Folder name"
            value={newFolderName}
            onChange={e => setNewFolderName(e.target.value)}
            onKeyDown={e => e.key === 'Enter' && handleCreateFolderSubmit()}
            autoFocus
          />
          <div style={{ display: 'flex', gap: '8px', justifyContent: 'flex-end' }}>
            <button className="btn btn-ghost btn-sm" onClick={() => setShowCreateFolder(false)}>Cancel</button>
            <button className="btn btn-primary btn-sm" onClick={handleCreateFolderSubmit} disabled={!newFolderName.trim()}>Create</button>
          </div>
        </div>
      )}

      {/* Rename Modal */}
      {renamingEntry && (
        <div className="glass-panel" style={{
          position: 'fixed', top: '50%', left: '50%', transform: 'translate(-50%, -50%)',
          zIndex: 1000, display: 'flex', flexDirection: 'column', gap: '16px',
          width: '320px', padding: '20px', background: 'rgba(12, 10, 20, 0.95)', border: '1px solid var(--border)'
        }}>
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
            <span style={{ fontWeight: 600, fontSize: '0.9rem' }}>Rename Item</span>
            <X size={16} style={{ cursor: 'pointer' }} onClick={() => setRenamingEntry(null)} />
          </div>
          <input
            type="text"
            value={newName}
            onChange={e => setNewName(e.target.value)}
            onKeyDown={e => e.key === 'Enter' && handleRenameSubmit()}
            autoFocus
          />
          <div style={{ display: 'flex', gap: '8px', justifyContent: 'flex-end' }}>
            <button className="btn btn-ghost btn-sm" onClick={() => setRenamingEntry(null)}>Cancel</button>
            <button className="btn btn-primary btn-sm" onClick={handleRenameSubmit} disabled={!newName.trim() || newName === renamingEntry.name}>Rename</button>
          </div>
        </div>
      )}

      {/* Delete Confirmation Modal */}
      {deletingEntry && (
        <div className="glass-panel" style={{
          position: 'fixed', top: '50%', left: '50%', transform: 'translate(-50%, -50%)',
          zIndex: 1000, display: 'flex', flexDirection: 'column', gap: '16px',
          width: '340px', padding: '20px', background: 'rgba(12, 10, 20, 0.95)', border: '1px solid var(--border)'
        }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: '10px', color: 'var(--danger)' }}>
            <AlertTriangle size={20} />
            <span style={{ fontWeight: 600, fontSize: '0.95rem' }}>Confirm Delete</span>
          </div>
          <p style={{ fontSize: '0.8rem', color: 'var(--text-secondary)', lineHeight: 1.4 }}>
            Are you sure you want to delete <strong>{deletingEntry.name}</strong>? This action is permanent and cannot be undone.
          </p>
          <div style={{ display: 'flex', gap: '8px', justifyContent: 'flex-end', marginTop: '4px' }}>
            <button className="btn btn-ghost btn-sm" onClick={() => setDeletingEntry(null)}>Cancel</button>
            <button className="btn btn-danger btn-sm" onClick={handleDeleteConfirm}>Delete Permanent</button>
          </div>
        </div>
      )}

    </div>
  );
}
