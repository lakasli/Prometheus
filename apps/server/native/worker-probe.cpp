// Windows isolation prerequisite probe. This does not accept user programs.
// References: Microsoft CreateAppContainerProfile, UpdateProcThreadAttribute,
// JOBOBJECT_EXTENDED_LIMIT_INFORMATION and CPU_RATE_CONTROL_INFORMATION.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <userenv.h>
#include <sddl.h>
#include <aclapi.h>
#include <filesystem>
#include <iostream>
#include <string>
#include <thread>
#include <vector>
#include <stdexcept>
#include <atomic>

struct Handle {
  HANDLE value = nullptr;
  Handle() = default;
  explicit Handle(HANDLE h) : value(h) {}
  Handle(const Handle&) = delete;
  Handle& operator=(const Handle&) = delete;
  ~Handle() { if (value && value != INVALID_HANDLE_VALUE) CloseHandle(value); }
};
struct Failure { const char* stage; DWORD code; };
void require(BOOL ok, const char* stage) {
  if (!ok) throw Failure{stage, GetLastError()};
}
std::wstring quote(const std::wstring& s) {
  // All arguments here are absolute filenames, decimal numbers or fixed words.
  if (s.find_first_of(L"\"\r\n") != std::wstring::npos || (!s.empty() && s.back() == L'\\'))
    throw Failure{"argument", ERROR_INVALID_PARAMETER};
  return L"\"" + s + L"\"";
}
bool accessible(const wchar_t* path, DWORD access) {
  Handle file(CreateFileW(path, access, FILE_SHARE_READ | FILE_SHARE_WRITE |
    FILE_SHARE_DELETE, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr));
  return file.value != INVALID_HANDLE_VALUE;
}
int networkError(const char* address, unsigned short port) {
  SOCKET s = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
  if (s == INVALID_SOCKET) return WSAGetLastError();
  u_long nonblocking = 1;
  ioctlsocket(s, FIONBIO, &nonblocking);
  sockaddr_in target{};
  target.sin_family = AF_INET;
  target.sin_port = htons(port);
  inet_pton(AF_INET, address, &target.sin_addr);
  int result = connect(s, reinterpret_cast<sockaddr*>(&target), sizeof(target));
  int error = result == 0 ? 0 : WSAGetLastError();
  if (error == WSAEWOULDBLOCK) {
    fd_set writes, errors;
    FD_ZERO(&writes); FD_ZERO(&errors);
    FD_SET(s, &writes); FD_SET(s, &errors);
    timeval timeout{2, 0};
    if (select(0, nullptr, &writes, &errors, &timeout) > 0) {
      int size = sizeof(error);
      getsockopt(s, SOL_SOCKET, SO_ERROR, reinterpret_cast<char*>(&error), &size);
    } else error = WSAETIMEDOUT;
  }
  closesocket(s);
  return error;
}
int child(int argc, wchar_t** argv) {
  if (argc != 7) return 90;
  const std::wstring mode = argv[2];
  if (mode == L"hang") { for (;;) Sleep(100); }
  if (mode == L"cpu") {
    std::vector<std::thread> threads;
    for (DWORD i=0;i<std::min<DWORD>(64,GetActiveProcessorCount(ALL_PROCESSOR_GROUPS));++i)
      threads.emplace_back([] { std::atomic<unsigned long long> n{0};for(;;)n.fetch_add(1,std::memory_order_relaxed); });
    for(auto& t:threads)t.join();
  }
  if (mode == L"exit") return 7;
  if (mode == L"flood") {
    std::string block(4096, 'x');
    for (int i = 0; i < 512; ++i) std::cout << block << std::flush;
    return 0;
  }
  if (mode != L"permissions") return 91;
  Handle token;
  require(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token.value), "child_token");
  DWORD app = 0, size = sizeof(app);
  require(GetTokenInformation(token.value, TokenIsAppContainer, &app, size, &size), "child_container");
  GetTokenInformation(token.value, TokenCapabilities, nullptr, 0, &size);
  std::vector<BYTE> caps(size);
  require(GetTokenInformation(token.value, TokenCapabilities, caps.data(), size, &size), "child_capabilities");
  auto groups = reinterpret_cast<TOKEN_GROUPS*>(caps.data());
  GetTokenInformation(token.value, TokenIntegrityLevel, nullptr, 0, &size);
  std::vector<BYTE> label(size);
  require(GetTokenInformation(token.value, TokenIntegrityLevel, label.data(), size, &size), "child_integrity");
  PSID sid = reinterpret_cast<TOKEN_MANDATORY_LABEL*>(label.data())->Label.Sid;
  DWORD integrity = *GetSidSubAuthority(sid, *GetSidSubAuthorityCount(sid) - 1);
  BOOL inJob = FALSE;
  require(IsProcessInJob(GetCurrentProcess(), nullptr, &inJob), "child_job");
  wchar_t env[16];
  bool envAbsent = GetEnvironmentVariableW(L"PROMETHEUS_PROBE_SECRET", env, 16) == 0;
  Handle parent(OpenProcess(PROCESS_VM_READ | PROCESS_VM_WRITE, FALSE, wcstoul(argv[5], nullptr, 10)));
  void* memory = VirtualAlloc(nullptr, 128ULL * 1024 * 1024, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
  bool memoryDenied = memory == nullptr;
  if (memory) VirtualFree(memory, 0, MEM_RELEASE);
  std::wstring executable(32768, L'\0');
  executable.resize(GetModuleFileNameW(nullptr, executable.data(), static_cast<DWORD>(executable.size())));
  std::wstring command = quote(executable) + L" --child exit ignored 1 1 1";
  STARTUPINFOW start{}; start.cb = sizeof(start);
  PROCESS_INFORMATION process{};
  BOOL spawned = CreateProcessW(executable.c_str(), command.data(), nullptr, nullptr,
    FALSE, CREATE_NO_WINDOW, nullptr, nullptr, &start, &process);
  if (spawned) { TerminateProcess(process.hProcess, 92); WaitForSingleObject(process.hProcess, 5000);
    CloseHandle(process.hThread); CloseHandle(process.hProcess); }
  WSADATA sockets{};
  require(WSAStartup(MAKEWORD(2, 2), &sockets) == 0, "winsock");
  int loopback = networkError("127.0.0.1", static_cast<unsigned short>(wcstoul(argv[4], nullptr, 10)));
  int publicNetwork = networkError("1.1.1.1", 443);
  WSACleanup();
  bool readDenied = !accessible(argv[3], GENERIC_READ);
  bool writeDenied = !accessible(argv[3], GENERIC_WRITE);
  bool sourceDenied = !accessible(argv[6], GENERIC_READ);
  std::cout << "{\"app_container\":" << app
    << ",\"capabilities\":" << groups->GroupCount << ",\"integrity_rid\":" << integrity
    << ",\"in_job\":" << inJob << ",\"private_read_denied\":" << readDenied
    << ",\"private_write_denied\":" << writeDenied << ",\"source_read_denied\":" << sourceDenied
    << ",\"parent_access_denied\":" << (parent.value == nullptr)
    << ",\"environment_absent\":" << envAbsent << ",\"memory_denied\":" << memoryDenied
    << ",\"child_process_denied\":" << !spawned
    << ",\"loopback_error\":" << loopback << ",\"public_network_error\":" << publicNetwork << "}" << std::endl;
  return 0;
}
#ifndef PROMETHEUS_PROBE_CHILD
std::filesystem::path extendedPath(const std::filesystem::path& path) {
  const auto value=path.wstring();
  require(path.is_absolute(),"absolute_path");
  if(value.rfind(L"\\\\?\\",0)==0)return path;
  if(value.rfind(L"\\\\",0)==0)return std::filesystem::path(L"\\\\?\\UNC\\"+value.substr(2));
  return std::filesystem::path(L"\\\\?\\"+value);
}
struct Profile {
  std::wstring name;
  PSID sid = nullptr;
  bool owned=false;
  ~Profile() {
    if (sid) FreeSid(sid);
    if (owned) {
      HRESULT result = DeleteAppContainerProfile(name.c_str());
      if (FAILED(result)) std::cout << "{\"event\":\"blocked\",\"stage\":\"profile_cleanup\",\"windows_error\":" << static_cast<DWORD>(result) << "}" << std::endl;
    }
  }
};
struct ProbeDirectory {
  std::filesystem::path directory;
  bool media = false;
  ~ProbeDirectory() {
    if (media) {
      // The caller created this UUID directory under its checked private root.
      // Never traverse a reparse point while removing untrusted worker output.
      auto remove = [](auto&& self, const std::filesystem::path& path) -> void {
        DWORD attr = GetFileAttributesW(path.c_str());
        if (attr == INVALID_FILE_ATTRIBUTES) return;
        if ((attr & FILE_ATTRIBUTE_DIRECTORY) && !(attr & FILE_ATTRIBUTE_REPARSE_POINT)) {
          WIN32_FIND_DATAW item{};HANDLE search=FindFirstFileW((path/L"*").c_str(),&item);
          if(search!=INVALID_HANDLE_VALUE){do{if(std::wstring(item.cFileName)!=L"."&&std::wstring(item.cFileName)!=L"..")self(self,path/item.cFileName);}while(FindNextFileW(search,&item));FindClose(search);}
        }
        if(attr&FILE_ATTRIBUTE_DIRECTORY)RemoveDirectoryW(path.c_str());else DeleteFileW(path.c_str());
      };
      remove(remove,directory);
      if(GetFileAttributesW(directory.c_str())!=INVALID_FILE_ATTRIBUTES)std::cout<<"{\"event\":\"blocked\",\"stage\":\"media_directory_cleanup\"}"<<std::endl;
      return;
    }
    const auto executable = directory / L"probe.exe";
    if (!DeleteFileW(executable.c_str()) && GetLastError() != ERROR_FILE_NOT_FOUND)
      std::cout << "{\"event\":\"blocked\",\"stage\":\"probe_file_cleanup\",\"windows_error\":" << GetLastError() << "}" << std::endl;
    if (!RemoveDirectoryW(directory.c_str()))
      std::cout << "{\"event\":\"blocked\",\"stage\":\"probe_directory_cleanup\",\"windows_error\":" << GetLastError() << "}" << std::endl;
  }
};
// Grant this invocation's AppContainer read/execute on its copied probe only.
void grantRead(const std::wstring& path, PSID sid, bool inherit=false, bool write=false) {
  PACL oldAcl = nullptr, newAcl = nullptr;
  PSECURITY_DESCRIPTOR descriptor = nullptr;
  DWORD result = GetNamedSecurityInfoW(path.c_str(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
    nullptr, nullptr, &oldAcl, nullptr, &descriptor);
  if (result) throw Failure{"read_acl", result};
  EXPLICIT_ACCESSW access{};
  access.grfAccessPermissions = GENERIC_READ | GENERIC_EXECUTE | (write?GENERIC_WRITE|DELETE:0);
  access.grfAccessMode = GRANT_ACCESS;
  access.grfInheritance = inherit?SUB_CONTAINERS_AND_OBJECTS_INHERIT:NO_INHERITANCE;
  access.Trustee.TrusteeForm = TRUSTEE_IS_SID;
  access.Trustee.TrusteeType = TRUSTEE_IS_UNKNOWN;
  access.Trustee.ptstrName = static_cast<LPWSTR>(sid);
  result = SetEntriesInAclW(1, &access, oldAcl, &newAcl);
  if (!result) result = SetNamedSecurityInfoW(const_cast<LPWSTR>(path.c_str()), SE_FILE_OBJECT,
    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, nullptr, nullptr, newAcl, nullptr);
  if (newAcl) LocalFree(newAcl);
  LocalFree(descriptor);
  if (result) throw Failure{"grant_acl", result};
}
int launcher(int argc, wchar_t** argv) {
  if(argc==4&&(std::wstring(argv[1])==L"--cleanup-media"||std::wstring(argv[1])==L"--cleanup-probe")){
    const std::filesystem::path root=extendedPath(argv[2]);const std::wstring id=argv[3];
    require(root.is_absolute()&&!(GetFileAttributesW(root.c_str())&FILE_ATTRIBUTE_REPARSE_POINT)&&id.size()==36&&id.find_first_not_of(L"0123456789abcdef-")==std::wstring::npos,"cleanup_boundary");
    {ProbeDirectory staged{root/id,true};}
    const std::wstring profile=(std::wstring(argv[1])==L"--cleanup-media"?L"Prometheus.Media.":L"Prometheus.Probe.")+id;
    HRESULT result=DeleteAppContainerProfile(profile.c_str());
    if(FAILED(result)&&result!=HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)&&result!=HRESULT_FROM_WIN32(ERROR_NOT_FOUND))throw Failure{"profile_cleanup",static_cast<DWORD>(result)};
    std::cout<<"{\"event\":\"cleanup\",\"status\":\"completed\"}"<<std::endl;return 0;
  }
  // root, unique profile id, mode, private sentinel, local listening port, source sentinel, timeout.
  if (argc != 9 || (std::wstring(argv[1]) != L"--probe" && std::wstring(argv[1]) != L"--media")) throw Failure{"arguments", ERROR_INVALID_PARAMETER};
  const bool media=std::wstring(argv[1])==L"--media";
  std::wstring root = argv[2], id = argv[3], mode = argv[4];
  if (id.size() != 36 || id.find_first_not_of(L"0123456789abcdef-") != std::wstring::npos ||
    (!media&&(mode != L"permissions" && mode != L"hang" && mode != L"exit" && mode != L"flood" && mode != L"cpu")) ||
    (media&&mode!=L"tts"&&mode!=L"asr"&&mode!=L"video"&&mode!=L"selftest"))
    throw Failure{"arguments", ERROR_INVALID_PARAMETER};
  DWORD timeout = wcstoul(argv[8], nullptr, 10);
  if (timeout < 100 || timeout > (media?60000UL:15000UL)) throw Failure{"timeout", ERROR_INVALID_PARAMETER};
  require(std::filesystem::path(root).is_absolute()&&!(GetFileAttributesW(root.c_str())&FILE_ATTRIBUTE_REPARSE_POINT),"root_boundary");
  root=extendedPath(root).wstring();
  auto directory = std::filesystem::path(root) / id;
  // The TypeScript caller creates and ACL-checks the root. Never follow a supplied run directory.
  require(CreateDirectoryW(directory.c_str(), nullptr), "create_run_directory");
  ProbeDirectory staged{directory,media};
  Profile profile;
  profile.name = (media?L"Prometheus.Media.":L"Prometheus.Probe.") + id;
  HRESULT hr = CreateAppContainerProfile(profile.name.c_str(), L"Prometheus isolation probe",
    L"Temporary empty-capability prerequisite test", nullptr, 0, &profile.sid);
  if (FAILED(hr)) throw Failure{"create_app_container", static_cast<DWORD>(hr)};
  profile.owned=true;
  std::wstring source(32768, L'\0');
  source.resize(GetModuleFileNameW(nullptr, source.data(), static_cast<DWORD>(source.size())));
  auto executable = directory / L"probe.exe";
  source = (std::filesystem::path(source).parent_path() / L"probe-child.exe").wstring();
  if(media){
    const std::filesystem::path bundle=extendedPath(argv[5]);
    require(bundle.is_absolute()&&!(GetFileAttributesW(bundle.c_str())&FILE_ATTRIBUTE_REPARSE_POINT),"bundle_boundary");
    for(const auto& entry:std::filesystem::recursive_directory_iterator(bundle))require(!(GetFileAttributesW(entry.path().c_str())&FILE_ATTRIBUTE_REPARSE_POINT),"bundle_reparse");
    std::filesystem::copy(bundle,directory/L"runtime",std::filesystem::copy_options::recursive);
    executable=directory/L"runtime"/L"pythonw.exe";
    grantRead((directory/L"runtime").wstring(),profile.sid,true);
    for(const wchar_t* name:{L"request.json",L"input.bin"}){
      require(CopyFileW((std::filesystem::path(root)/L".requests"/id/name).c_str(),(directory/name).c_str(),TRUE),"copy_input");
      grantRead((directory/name).wstring(),profile.sid);
    }
    require(CreateDirectoryW((directory/L"output").c_str(),nullptr),"output_directory");
    grantRead((directory/L"output").wstring(),profile.sid,true,true);
    PSECURITY_DESCRIPTOR low=nullptr;require(ConvertStringSecurityDescriptorToSecurityDescriptorW(L"S:(ML;;NW;;;LW)",SDDL_REVISION_1,&low,nullptr),"output_label_descriptor");
    BOOL present=FALSE,defaulted=FALSE;PACL sacl=nullptr;require(GetSecurityDescriptorSacl(low,&present,&sacl,&defaulted),"output_label");
    DWORD labeled=SetNamedSecurityInfoW(const_cast<LPWSTR>((directory/L"output").c_str()),SE_FILE_OBJECT,LABEL_SECURITY_INFORMATION,nullptr,nullptr,nullptr,sacl);LocalFree(low);if(labeled)throw Failure{"set_output_label",labeled};
  }else require(CopyFileW(source.c_str(), executable.c_str(), TRUE), "copy_probe");
  grantRead(directory.wstring(), profile.sid);
  grantRead(executable.wstring(), profile.sid);
  Handle job(CreateJobObjectW(nullptr, nullptr));
  require(job.value != nullptr, "create_job");
  JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
  limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE |
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY |
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
  limits.BasicLimitInformation.ActiveProcessLimit = 1;
  limits.ProcessMemoryLimit = limits.JobMemoryLimit = (media?1536ULL:64ULL) * 1024 * 1024;
  require(SetInformationJobObject(job.value, JobObjectExtendedLimitInformation, &limits, sizeof(limits)), "job_limits");
  JOBOBJECT_CPU_RATE_CONTROL_INFORMATION cpu{};
  cpu.ControlFlags = JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
  cpu.CpuRate = 2000;
  require(SetInformationJobObject(job.value, JobObjectCpuRateControlInformation, &cpu, sizeof(cpu)), "job_cpu");
  JOBOBJECT_BASIC_UI_RESTRICTIONS ui{}; ui.UIRestrictionsClass = JOB_OBJECT_UILIMIT_ALL;
  require(SetInformationJobObject(job.value, JobObjectBasicUIRestrictions, &ui, sizeof(ui)), "job_ui");
  SECURITY_ATTRIBUTES inherit{sizeof(inherit), nullptr, TRUE};
  Handle outputRead, outputWrite;
  require(CreatePipe(&outputRead.value, &outputWrite.value, &inherit, 0), "stdout_pipe");
  require(SetHandleInformation(outputRead.value, HANDLE_FLAG_INHERIT, 0), "stdout_inheritance");
  Handle input(CreateFileW(L"NUL", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &inherit,
    OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr));
  require(input.value != INVALID_HANDLE_VALUE, "null_input");
  SIZE_T size = 0;
  InitializeProcThreadAttributeList(nullptr, 4, 0, &size);
  std::vector<BYTE> storage(size);
  auto attributes = reinterpret_cast<LPPROC_THREAD_ATTRIBUTE_LIST>(storage.data());
  require(InitializeProcThreadAttributeList(attributes, 4, 0, &size), "attributes");
  struct AttributeCleanup { LPPROC_THREAD_ATTRIBUTE_LIST list; ~AttributeCleanup() { DeleteProcThreadAttributeList(list); } } cleanup{attributes};
  SECURITY_CAPABILITIES capabilities{}; capabilities.AppContainerSid = profile.sid;
  DWORD childPolicy = PROCESS_CREATION_CHILD_PROCESS_RESTRICTED;
  HANDLE handles[] = {input.value, outputWrite.value};
  require(UpdateProcThreadAttribute(attributes, 0, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    &capabilities, sizeof(capabilities), nullptr, nullptr), "container_attribute");
  require(UpdateProcThreadAttribute(attributes, 0, PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
    &childPolicy, sizeof(childPolicy), nullptr, nullptr), "child_policy");
  require(UpdateProcThreadAttribute(attributes, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    handles, sizeof(handles), nullptr, nullptr), "handle_list");
  require(UpdateProcThreadAttribute(attributes, 0, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    &job.value, sizeof(job.value), nullptr, nullptr), "job_attribute");
  wchar_t systemRoot[32768];
  require(GetWindowsDirectoryW(systemRoot, 32768) != 0, "windows_directory");
  std::wstring environment;
  for (const wchar_t* name : {L"ALLUSERSPROFILE", L"APPDATA", L"LOCALAPPDATA", L"ProgramData", L"SystemDrive", L"SystemRoot", L"TEMP", L"TMP", L"USERPROFILE", L"WINDIR"}) {
    wchar_t value[32768];
    DWORD length = GetEnvironmentVariableW(name, value, 32768);
    if (length > 0 && length < 32768) environment += std::wstring(name) + L"=" + value + L'\0';
  }
  environment += L'\0';
  std::wstring command = quote(executable.wstring()) + L" --child " + mode + L" " + quote(argv[5]) + L" " + argv[6] +
    L" " + std::to_wstring(GetCurrentProcessId()) + L" " + quote(argv[7]);
  if(media)command=quote(executable.wstring())+L" -I -S -B "+quote((directory/L"runtime"/L"media-worker.py").wstring())+L" "+quote(directory.wstring());
  STARTUPINFOEXW start{}; start.StartupInfo.cb = sizeof(start);
  start.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
  start.StartupInfo.hStdInput = input.value;
  start.StartupInfo.hStdOutput = start.StartupInfo.hStdError = outputWrite.value;
  start.lpAttributeList = attributes;
  PROCESS_INFORMATION process{};
  require(CreateProcessW(executable.c_str(), command.data(), nullptr, nullptr, TRUE,
    EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | CREATE_SUSPENDED,
    environment.data(), directory.c_str(), &start.StartupInfo, &process), "create_isolated_process");
  Handle worker(process.hProcess), thread(process.hThread);
  struct JobExit {
    HANDLE job,worker;
    ~JobExit(){TerminateJobObject(job,94);WaitForSingleObject(worker,5000);JOBOBJECT_BASIC_ACCOUNTING_INFORMATION a{};for(int n=0;n<500;n++){if(!QueryInformationJobObject(job,JobObjectBasicAccountingInformation,&a,sizeof(a),nullptr)||a.ActiveProcesses==0)break;Sleep(10);}}
  } jobExit{job.value,worker.value};
  CloseHandle(outputWrite.value); outputWrite.value = nullptr;
  BOOL belongs = FALSE;
  require(IsProcessInJob(worker.value, job.value, &belongs) && belongs, "verify_job");
  JOBOBJECT_EXTENDED_LIMIT_INFORMATION actual{};
  JOBOBJECT_CPU_RATE_CONTROL_INFORMATION actualCpu{};
  require(QueryInformationJobObject(job.value, JobObjectExtendedLimitInformation, &actual, sizeof(actual), nullptr), "query_limits");
  require(QueryInformationJobObject(job.value, JobObjectCpuRateControlInformation, &actualCpu, sizeof(actualCpu), nullptr), "query_cpu");
  require(actual.BasicLimitInformation.LimitFlags == limits.BasicLimitInformation.LimitFlags &&
    actual.BasicLimitInformation.ActiveProcessLimit == 1 && actual.JobMemoryLimit == limits.JobMemoryLimit &&
    actual.ProcessMemoryLimit == limits.ProcessMemoryLimit && actualCpu.ControlFlags == cpu.ControlFlags && actualCpu.CpuRate == cpu.CpuRate,
    "verify_limits");
  require(ResumeThread(thread.value) != static_cast<DWORD>(-1), "resume");
  std::cout << "{\"event\":\"started\",\"pid\":" << process.dwProcessId
    << ",\"memory_bytes\":" << actual.JobMemoryLimit << ",\"cpu_rate\":" << actualCpu.CpuRate
    << ",\"active_process_limit\":1}" << std::endl;
  ULONGLONG deadline = GetTickCount64() + timeout;
  std::string result, reason = "exited";
  HANDLE control = GetStdHandle(STD_INPUT_HANDLE);
  for (;;) {
    if(media){
      unsigned long long total=0;DWORD count=0;WIN32_FIND_DATAW item{};HANDLE search=FindFirstFileW((directory/L"output"/L"*").c_str(),&item);
      if(search!=INVALID_HANDLE_VALUE){do{if(std::wstring(item.cFileName)==L"."||std::wstring(item.cFileName)==L"..")continue;count++;if(item.dwFileAttributes&(FILE_ATTRIBUTE_REPARSE_POINT|FILE_ATTRIBUTE_DIRECTORY)){total=~0ULL;break;}total+=(static_cast<unsigned long long>(item.nFileSizeHigh)<<32)|item.nFileSizeLow;}while(FindNextFileW(search,&item));FindClose(search);}
      if(total>8ULL*1024*1024||count>16){reason="file_limit";break;}
    }
    DWORD available = 0;
    if (PeekNamedPipe(outputRead.value, nullptr, 0, nullptr, &available, nullptr) && available) {
      char buffer[4096]; DWORD read = 0;
      require(ReadFile(outputRead.value, buffer, std::min<DWORD>(sizeof(buffer), available), &read, nullptr), "read_output");
      result.append(buffer, read);
      if (result.size() > 16384) { reason = "output_limit"; break; }
      continue;
    }
    if (WaitForSingleObject(worker.value, 0) == WAIT_OBJECT_0) break;
    if (!PeekNamedPipe(control, nullptr, 0, nullptr, &available, nullptr) || available) { reason = "cancelled"; break; }
    if (GetTickCount64() >= deadline) { reason = "timeout"; break; }
    Sleep(10);
  }
  require(TerminateJobObject(job.value, reason == "exited" ? 0 : 93), "terminate_job");
  require(WaitForSingleObject(worker.value, 5000) == WAIT_OBJECT_0, "wait_exit");
  JOBOBJECT_BASIC_ACCOUNTING_INFORMATION accounting{};
  for (int retry = 0; retry < 500; ++retry) {
    require(QueryInformationJobObject(job.value, JobObjectBasicAccountingInformation, &accounting, sizeof(accounting), nullptr), "query_exit");
    if (accounting.ActiveProcesses == 0) break;
    Sleep(10);
  }
  require(accounting.ActiveProcesses == 0, "job_not_empty");
  DWORD exitCode = 0; require(GetExitCodeProcess(worker.value, &exitCode), "exit_code");
  if(media && reason=="exited" && exitCode==0){
    const auto destination=std::filesystem::path(root)/L".requests"/id/L"output";
    require(CreateDirectoryW(destination.c_str(),nullptr),"host_output_directory");
    unsigned long long total=0;DWORD count=0;
    for(const auto& entry:std::filesystem::directory_iterator(directory/L"output")){
      const auto name=entry.path().filename().wstring();DWORD attr=GetFileAttributesW(entry.path().c_str());
      require(!(attr&(FILE_ATTRIBUTE_REPARSE_POINT|FILE_ATTRIBUTE_DIRECTORY)),"output_file_type");
      require(name==L"result.json"||name==L"speech.wav"||name==L"speech.mp3"||(name.size()==11&&name.substr(0,6)==L"frame-"&&name[6]>=L'0'&&name[6]<=L'5'&&name.substr(7)==L".png"),"output_file_name");
      total+=entry.file_size();count++;require(total<=8ULL*1024*1024&&count<=8,"output_file_size");
      require(CopyFileW(entry.path().c_str(),(destination/name).c_str(),TRUE),"host_output_copy");
    }
  }
  require(QueryInformationJobObject(job.value, JobObjectExtendedLimitInformation, &actual, sizeof(actual), nullptr), "query_peak");
  while (!result.empty() && (result.back() == '\r' || result.back() == '\n' || result.back() == ' ' || result.back() == '\t')) result.pop_back();
  bool probeJson = reason == "exited" && mode == L"permissions" && exitCode == 0 &&
    !result.empty() && result.front() == '{';
  std::cout << "{\"event\":\"finished\",\"reason\":\"" << reason << "\",\"exit_code\":" << exitCode
    << ",\"active_processes\":" << accounting.ActiveProcesses << ",\"cpu_100ns\":" << (accounting.TotalUserTime.QuadPart+accounting.TotalKernelTime.QuadPart)
    << ",\"peak_memory_bytes\":" << actual.PeakJobMemoryUsed << ",\"probe\":"
    << (probeJson ? result : "null") << "}" << std::endl;
  return 0;
}
#endif
int wmain(int argc, wchar_t** argv) {
  try {
#ifdef PROMETHEUS_PROBE_CHILD
    if (argc > 1 && std::wstring(argv[1]) == L"--child") return child(argc, argv);
    return 90;
#else
    return launcher(argc, argv);
#endif
  } catch (const Failure& error) {
    std::cout << "{\"event\":\"blocked\",\"stage\":\"" << error.stage << "\",\"windows_error\":" << error.code << "}" << std::endl;
    return 1;
  } catch (const std::filesystem::filesystem_error& error) {
    std::cout << "{\"event\":\"blocked\",\"stage\":\"filesystem\",\"windows_error\":" << error.code().value() << "}" << std::endl;
    return 1;
  } catch (...) {
    std::cout << "{\"event\":\"blocked\",\"stage\":\"unexpected\"}" << std::endl;
    return 1;
  }
}
