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
struct Profile {
  std::wstring name;
  PSID sid = nullptr;
  ~Profile() {
    if (sid) FreeSid(sid);
    if (!name.empty()) {
      HRESULT result = DeleteAppContainerProfile(name.c_str());
      if (FAILED(result)) std::cout << "{\"event\":\"blocked\",\"stage\":\"profile_cleanup\",\"windows_error\":" << static_cast<DWORD>(result) << "}" << std::endl;
    }
  }
};
struct ProbeDirectory {
  std::filesystem::path directory;
  ~ProbeDirectory() {
    const auto executable = directory / L"probe.exe";
    if (!DeleteFileW(executable.c_str()) && GetLastError() != ERROR_FILE_NOT_FOUND)
      std::cout << "{\"event\":\"blocked\",\"stage\":\"probe_file_cleanup\",\"windows_error\":" << GetLastError() << "}" << std::endl;
    if (!RemoveDirectoryW(directory.c_str()))
      std::cout << "{\"event\":\"blocked\",\"stage\":\"probe_directory_cleanup\",\"windows_error\":" << GetLastError() << "}" << std::endl;
  }
};
// Grant this invocation's AppContainer read/execute on its copied probe only.
void grantRead(const std::wstring& path, PSID sid) {
  PACL oldAcl = nullptr, newAcl = nullptr;
  PSECURITY_DESCRIPTOR descriptor = nullptr;
  DWORD result = GetNamedSecurityInfoW(path.c_str(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION,
    nullptr, nullptr, &oldAcl, nullptr, &descriptor);
  if (result) throw Failure{"read_acl", result};
  EXPLICIT_ACCESSW access{};
  access.grfAccessPermissions = GENERIC_READ | GENERIC_EXECUTE;
  access.grfAccessMode = GRANT_ACCESS;
  access.grfInheritance = NO_INHERITANCE;
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
  // root, unique profile id, mode, private sentinel, local listening port, source sentinel, timeout.
  if (argc != 9 || std::wstring(argv[1]) != L"--probe") throw Failure{"arguments", ERROR_INVALID_PARAMETER};
  std::wstring root = argv[2], id = argv[3], mode = argv[4];
  if (id.size() != 36 || id.find_first_not_of(L"0123456789abcdef-") != std::wstring::npos ||
    (mode != L"permissions" && mode != L"hang" && mode != L"exit" && mode != L"flood"))
    throw Failure{"arguments", ERROR_INVALID_PARAMETER};
  DWORD timeout = wcstoul(argv[8], nullptr, 10);
  if (timeout < 100 || timeout > 15000) throw Failure{"timeout", ERROR_INVALID_PARAMETER};
  auto directory = std::filesystem::path(root) / id;
  // The TypeScript caller creates and ACL-checks the root. Never follow a supplied run directory.
  require(CreateDirectoryW(directory.c_str(), nullptr), "create_run_directory");
  ProbeDirectory staged{directory};
  Profile profile;
  profile.name = L"Prometheus.Probe." + id;
  HRESULT hr = CreateAppContainerProfile(profile.name.c_str(), L"Prometheus isolation probe",
    L"Temporary empty-capability prerequisite test", nullptr, 0, &profile.sid);
  if (FAILED(hr)) throw Failure{"create_app_container", static_cast<DWORD>(hr)};
  std::wstring source(32768, L'\0');
  source.resize(GetModuleFileNameW(nullptr, source.data(), static_cast<DWORD>(source.size())));
  auto executable = directory / L"probe.exe";
  source = (std::filesystem::path(source).parent_path() / L"probe-child.exe").wstring();
  require(CopyFileW(source.c_str(), executable.c_str(), TRUE), "copy_probe");
  grantRead(directory.wstring(), profile.sid);
  grantRead(executable.wstring(), profile.sid);
  Handle job(CreateJobObjectW(nullptr, nullptr));
  require(job.value != nullptr, "create_job");
  JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
  limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE |
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY |
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
  limits.BasicLimitInformation.ActiveProcessLimit = 1;
  limits.ProcessMemoryLimit = limits.JobMemoryLimit = 64ULL * 1024 * 1024;
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
  bool probeJson = reason == "exited" && mode == L"permissions" && exitCode == 0 &&
    !result.empty() && result.front() == '{';
  std::cout << "{\"event\":\"finished\",\"reason\":\"" << reason << "\",\"exit_code\":" << exitCode
    << ",\"active_processes\":" << accounting.ActiveProcesses << ",\"probe\":"
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
  } catch (...) {
    std::cout << "{\"event\":\"blocked\",\"stage\":\"unexpected\"}" << std::endl;
    return 1;
  }
}
