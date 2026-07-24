use crate::{ProjectRoot, WorkspaceServiceError};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use relaycat_protocol::{ShellDescriptor, ShellSnapshot, WorkspaceErrorCode};
use std::{
    collections::{HashMap, VecDeque},
    env,
    io::{Read, Write},
    sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}},
    thread,
};

const OUTPUT_CHUNK: usize = 32 * 1024;
const OUTPUT_RING_LIMIT: usize = 2 * 1024 * 1024;
static NEXT_SHELL_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct ShellManager { root: ProjectRoot, limit: u8, shells: Arc<Mutex<HashMap<String, Arc<Shell>>>> }

struct Shell {
    descriptor: Mutex<ShellDescriptor>,
    last_input_seq: Mutex<u64>,
    output: Mutex<OutputRing>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
}

#[derive(Default)]
struct OutputRing { entries: VecDeque<(u64, Vec<u8>)>, bytes: usize }

impl OutputRing {
    fn push(&mut self, seq: u64, bytes: Vec<u8>) {
        self.bytes += bytes.len(); self.entries.push_back((seq, bytes));
        while self.bytes > OUTPUT_RING_LIMIT { if let Some((_, old))=self.entries.pop_front(){self.bytes=self.bytes.saturating_sub(old.len());}else{break;} }
    }
    fn snapshot(&self, after: u64) -> (u64, u64, Vec<u8>, bool) {
        let first = self.entries.front().map(|(seq,_)|*seq).unwrap_or(0);
        let last = self.entries.back().map(|(seq,_)|*seq).unwrap_or(0);
        let complete = after == 0 || first == 0 || after.saturating_add(1) >= first;
        let bytes = self.entries.iter().filter(|(seq,_)|*seq>after).flat_map(|(_,data)|data.iter().copied()).collect();
        (first,last,bytes,complete)
    }
}

impl ShellManager {
    pub fn new(root: ProjectRoot, limit: u8) -> Self { Self { root, limit, shells: Arc::new(Mutex::new(HashMap::new())) } }

    pub fn list(&self) -> Vec<ShellDescriptor> {
        let shells=self.shells.lock().expect("shell map poisoned");
        let mut list:Vec<_>=shells.values().map(|shell|shell.descriptor.lock().expect("descriptor poisoned").clone()).collect();
        list.sort_by(|a,b|a.title.cmp(&b.title)); list
    }

    pub fn create(&self, cols:u16, rows:u16) -> Result<ShellSnapshot,WorkspaceServiceError> {
        let mut shells=self.shells.lock().map_err(|_|WorkspaceServiceError::busy())?;
        if shells.len()>=usize::from(self.limit){return Err(WorkspaceServiceError::busy());}
        let numeric=NEXT_SHELL_ID.fetch_add(1,Ordering::Relaxed);
        let id=format!("shell-{numeric}"); let title=format!("Shell {}",shells.len()+1);
        let pair=native_pty_system().openpty(PtySize{rows:rows.max(1),cols:cols.max(1),pixel_width:0,pixel_height:0}).map_err(|e|WorkspaceServiceError::new(WorkspaceErrorCode::Internal,e.to_string(),true))?;
        let program=default_shell(); let mut command=CommandBuilder::new(program); command.cwd(self.root.path());
        command.env("TERM","xterm-256color");
        let child=pair.slave.spawn_command(command).map_err(|e|WorkspaceServiceError::new(WorkspaceErrorCode::Internal,e.to_string(),true))?;
        drop(pair.slave);
        let mut reader=pair.master.try_clone_reader().map_err(|e|WorkspaceServiceError::new(WorkspaceErrorCode::Internal,e.to_string(),true))?;
        let writer=pair.master.take_writer().map_err(|e|WorkspaceServiceError::new(WorkspaceErrorCode::Internal,e.to_string(),true))?;
        let descriptor=ShellDescriptor{shell_id:id.clone(),title,cols:cols.max(1),rows:rows.max(1),last_output_seq:0,exited:false,exit_code:None};
        let shell=Arc::new(Shell{descriptor:Mutex::new(descriptor.clone()),last_input_seq:Mutex::new(0),output:Mutex::new(OutputRing::default()),writer:Mutex::new(writer),master:Mutex::new(pair.master),child:Mutex::new(child)});
        let background=Arc::clone(&shell);
        thread::Builder::new().name(format!("relaycat-{id}-reader")).spawn(move||read_output(&mut reader,&background)).map_err(WorkspaceServiceError::io)?;
        shells.insert(id,Arc::clone(&shell));
        Ok(ShellSnapshot{descriptor,first_output_seq:0,last_output_seq:0,bytes:Vec::new(),complete_screen:true})
    }

    pub fn input(&self,id:&str,seq:u64,bytes:Vec<u8>)->Result<(),WorkspaceServiceError>{
        let shell=self.shell(id)?; let mut last=shell.last_input_seq.lock().map_err(|_|WorkspaceServiceError::busy())?;
        if seq<=*last{return Ok(());} if seq!=last.saturating_add(1){return Err(WorkspaceServiceError::invalid("shell input sequence gap"));}
        shell.writer.lock().map_err(|_|WorkspaceServiceError::busy())?.write_all(&bytes).map_err(WorkspaceServiceError::io)?; *last=seq; Ok(())
    }
    pub fn resize(&self,id:&str,cols:u16,rows:u16)->Result<(),WorkspaceServiceError>{let shell=self.shell(id)?;shell.master.lock().map_err(|_|WorkspaceServiceError::busy())?.resize(PtySize{rows:rows.max(1),cols:cols.max(1),pixel_width:0,pixel_height:0}).map_err(|e|WorkspaceServiceError::new(WorkspaceErrorCode::Internal,e.to_string(),true))?;let mut d=shell.descriptor.lock().map_err(|_|WorkspaceServiceError::busy())?;d.cols=cols.max(1);d.rows=rows.max(1);Ok(())}
    pub fn snapshot(&self,id:&str,after:u64)->Result<ShellSnapshot,WorkspaceServiceError>{let shell=self.shell(id)?;let descriptor=shell.descriptor.lock().map_err(|_|WorkspaceServiceError::busy())?.clone();let(first,last,bytes,complete)=shell.output.lock().map_err(|_|WorkspaceServiceError::busy())?.snapshot(after);Ok(ShellSnapshot{descriptor,first_output_seq:first,last_output_seq:last,bytes,complete_screen:complete})}
    pub fn close(&self,id:&str)->Result<(),WorkspaceServiceError>{let shell=self.shells.lock().map_err(|_|WorkspaceServiceError::busy())?.remove(id).ok_or_else(||WorkspaceServiceError::new(WorkspaceErrorCode::NotFound,"shell was not found",false))?;let _=shell.child.lock().map_err(|_|WorkspaceServiceError::busy())?.kill();Ok(())}
    pub fn close_all(&self)->Result<(),WorkspaceServiceError>{let ids:Vec<String>=self.shells.lock().map_err(|_|WorkspaceServiceError::busy())?.keys().cloned().collect();for id in ids{self.close(&id)?;}Ok(())}
    fn shell(&self,id:&str)->Result<Arc<Shell>,WorkspaceServiceError>{self.shells.lock().map_err(|_|WorkspaceServiceError::busy())?.get(id).cloned().ok_or_else(||WorkspaceServiceError::new(WorkspaceErrorCode::NotFound,"shell was not found",false))}
}

impl Drop for ShellManager { fn drop(&mut self) { if Arc::strong_count(&self.shells)==1 { let _=self.close_all(); } } }

fn read_output(reader:&mut dyn Read,shell:&Arc<Shell>){let mut buffer=vec![0u8;OUTPUT_CHUNK];loop{match reader.read(&mut buffer){Ok(0)|Err(_)=>{if let Ok(mut d)=shell.descriptor.lock(){d.exited=true;}break}Ok(count)=>{let seq=if let Ok(mut d)=shell.descriptor.lock(){d.last_output_seq=d.last_output_seq.saturating_add(1);d.last_output_seq}else{break};if let Ok(mut ring)=shell.output.lock(){ring.push(seq,buffer[..count].to_vec());}}}}}
fn default_shell()->String{if cfg!(windows){env::var("COMSPEC").unwrap_or_else(|_|"cmd.exe".into())}else{env::var("SHELL").unwrap_or_else(|_|"/bin/sh".into())}}
